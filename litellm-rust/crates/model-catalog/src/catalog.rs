use std::borrow::Cow;
use std::collections::HashSet;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Loaded, Located, Rejects, pointer_child, pointer_for_key};
use crate::fallback::{FallbackGeneralizations, FallbackRule};
use crate::model::Model;
use crate::store::{Rows, Store};
use crate::wire::{EntryWire, Fields};

const SAMPLE_SPEC: &str = "sample_spec";
const FALLBACK_GENERALIZATIONS: &str = "fallback_generalizations";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntegrityLimits {
    min_model_count: usize,
    min_reference_ratio: f64,
}

#[derive(Clone, Debug)]
pub struct ModelEntry {
    fields: Fields,
    model: Option<Model>,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelMatch<'a> {
    pub matched_key: &'a str,
    pub canonical_key: &'a str,
    pub entry: &'a ModelEntry,
}

#[derive(Clone, Debug)]
struct Section {
    index: usize,
    key: String,
    value: Value,
}

/// One fetched key kept off the served map because its row failed validation
#[derive(Clone, Debug)]
pub struct Rejection<'a> {
    pub key: String,
    pub errors: Vec<&'a Located>,
    /// The bundled row for the key is served instead. `false` means the key is left out
    pub replaced_by_bundled: bool,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(untagged)]
pub enum Written<'a> {
    Entry(&'a Fields),
    Section(&'a Value),
}

#[derive(Clone, Debug)]
pub struct Catalog {
    store: Store<CatalogRows>,
    sections: Vec<Section>,
    fallback_generalizations: Option<FallbackGeneralizations>,
}

/// Models and aliases, an alias served as its owner's row
#[derive(Clone, Debug)]
struct CatalogRows {
    entries: IndexMap<String, ModelEntry>,
    aliases: IndexMap<String, String>,
}

impl CatalogRows {
    fn owner<'a>(&'a self, key: &'a str) -> &'a str {
        self.aliases.get(key).map_or(key, String::as_str)
    }

    fn fields(&self, key: &str) -> Option<&Fields> {
        Some(self.entries.get(self.owner(key))?.fields())
    }
}

/// Model names match ignoring case, and keys are grouped by their raw `litellm_provider`
impl Rows for CatalogRows {
    type Row = Fields;
    type Stamp = ();

    fn fold(name: &str) -> String {
        name.to_lowercase()
    }

    fn stamp(&self) -> Self::Stamp {}

    fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key) || self.aliases.contains_key(key)
    }

    fn row(&self, key: &str) -> Option<Cow<'_, Fields>> {
        self.fields(key).map(Cow::Borrowed)
    }

    fn scan(&self, visit: &mut dyn FnMut(&str, Option<&str>)) {
        let keys = self.entries.keys().chain(self.aliases.keys());
        for key in keys {
            let provider = self
                .fields(key)
                .and_then(|fields| fields.get("litellm_provider")?.as_str());
            visit(key, provider);
        }
    }
}

impl IntegrityLimits {
    pub fn new(min_model_count: usize, min_reference_ratio: f64) -> Option<Self> {
        (0.0..=1.0).contains(&min_reference_ratio).then_some(Self {
            min_model_count,
            min_reference_ratio,
        })
    }
}

impl ModelEntry {
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields.get(name)
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    pub fn model(&self) -> Option<&Model> {
        self.model.as_ref()
    }
}

impl Catalog {
    pub fn parse(body: &[u8]) -> Result<Loaded<Self>, Located> {
        let document = serde_json::from_slice::<Fields>(body)
            .map_err(|error| Located::new("", Error::Json(error)))?;

        let mut problems = Vec::new();
        let mut sections = Vec::new();
        let mut entries = IndexMap::with_capacity(document.len());
        let mut alias_lists = Vec::new();
        for (index, (name, value)) in document.into_iter().enumerate() {
            if value.is_null() && !is_section(&name) {
                sections.push(Section {
                    index,
                    key: name,
                    value,
                });
                continue;
            }
            let fields = match value {
                Value::Object(fields) if !is_section(&name) => fields,
                value => {
                    if !is_section(&name) {
                        problems.push(Located::from_key(&name, Error::NotAnObject));
                    }
                    sections.push(Section {
                        index,
                        key: name,
                        value,
                    });
                    continue;
                }
            };
            let (model, aliases, mut row_problems) = read_row(&fields);
            row_problems
                .iter_mut()
                .for_each(|problem| problem.prepend(&name));
            problems.extend(row_problems);
            if let Some(aliases) = aliases {
                alias_lists.push((name.clone(), aliases));
            }
            entries.insert(name, ModelEntry { fields, model });
        }
        if entries.is_empty() {
            return Err(Located::new("", Error::Empty));
        }

        let mut aliases: IndexMap<String, String> = IndexMap::new();
        for (model, names) in alias_lists {
            for alias in names {
                if entries.contains_key(&alias) {
                    problems.push(Located::from_key(
                        &model,
                        Error::AliasShadowsModel { alias },
                    ));
                } else if let Some(taken_by) = aliases.get(&alias) {
                    problems.push(Located::from_key(
                        &model,
                        Error::AliasTaken {
                            alias,
                            taken_by: taken_by.clone(),
                        },
                    ));
                } else {
                    aliases.insert(alias, model.clone());
                }
            }
        }

        let fallback_generalizations = sections
            .iter()
            .find(|section| section.key == FALLBACK_GENERALIZATIONS)
            .and_then(|section| {
                FallbackGeneralizations::deserialize(&section.value)
                    .map_err(|error| {
                        problems.push(Located::from_key(&section.key, Error::Invalid(error)))
                    })
                    .ok()
            });
        Ok(Loaded {
            value: Self {
                store: Store::new(CatalogRows { entries, aliases }),
                sections,
                fallback_generalizations,
            },
            errors: problems,
        })
    }

    /// Everything a cost map change has to fix before it merges, which is stricter than what `parse` serves
    pub fn problems(body: &[u8]) -> Vec<Located> {
        let Loaded {
            value: catalog,
            errors,
        } = match Self::parse(body) {
            Ok(loaded) => loaded,
            Err(fatal) => return vec![fatal],
        };
        let unread = catalog.unrecognized().map(|(model, key)| {
            Located::new(
                pointer_child(&pointer_for_key(model), key),
                Error::UnreadField,
            )
        });
        let no_fallback_rules = catalog
            .fallback_rules()
            .is_none_or(<[_]>::is_empty)
            .then(|| Located::from_key(FALLBACK_GENERALIZATIONS, Error::NoFallbackRules));
        errors
            .into_iter()
            .chain(unread)
            .chain(no_fallback_rules)
            .collect()
    }

    pub fn row_errors(model: &str, fields: &Fields) -> Vec<Located> {
        let (_, _, mut errors) = read_row(fields);
        errors.iter_mut().for_each(|error| error.prepend(model));
        errors
    }

    pub fn written(&self) -> impl Iterator<Item = (&str, Written<'_>)> {
        let mut sections = self.sections.iter().peekable();
        let mut entries = self.rows().entries.iter();
        (0..self.rows().entries.len() + self.sections.len()).map_while(move |index| match sections
            .next_if(|section| section.index == index)
        {
            Some(section) => Some((section.key.as_str(), Written::Section(&section.value))),
            None => entries
                .next()
                .map(|(name, entry)| (name.as_str(), Written::Entry(&entry.fields))),
        })
    }

    pub fn served(&self) -> impl Iterator<Item = (&str, &Fields)> {
        let rows = self.rows();
        rows.entries
            .iter()
            .map(|(name, entry)| (name.as_str(), entry.fields()))
            .chain(
                rows.aliases
                    .iter()
                    .map(|(alias, owner)| (alias.as_str(), rows.entries[owner].fields())),
            )
    }

    pub fn check_size(&self, reference: usize, limits: IntegrityLimits) -> Result<(), Located> {
        let actual = self.model_count();
        if actual < limits.min_model_count {
            return Err(Located::new(
                "",
                Error::BelowMinimum {
                    actual,
                    minimum: limits.min_model_count,
                },
            ));
        }
        if (actual as f64) < (reference as f64) * limits.min_reference_ratio {
            return Err(Located::new(
                "",
                Error::Shrunk {
                    actual,
                    reference,
                    ratio: limits.min_reference_ratio,
                },
            ));
        }
        Ok(())
    }

    pub fn without_rejected(self, errors: &[Located]) -> (Self, Vec<&Located>) {
        let row_names: HashSet<String> = self
            .rows()
            .entries
            .keys()
            .cloned()
            .chain(
                self.sections
                    .iter()
                    .filter(|section| !is_section(&section.key))
                    .map(|section| section.key.clone()),
            )
            .collect();
        let rejected: HashSet<String> = errors
            .iter()
            .filter(|error| error.error.rejects() == Rejects::Row)
            .filter_map(|error| crate::error::pointer_root(&error.path))
            .filter(|root| row_names.contains(root))
            .collect();
        let removed_errors = errors
            .iter()
            .filter(|error| {
                crate::error::pointer_root(&error.path).is_some_and(|root| rejected.contains(&root))
            })
            .collect();
        let removed_positions: HashSet<usize> = self
            .written()
            .enumerate()
            .filter(|(_, (key, _))| rejected.contains(*key))
            .map(|(index, _)| index)
            .collect();
        let mut catalog = self;
        catalog.store.edit(|rows| {
            rows.entries.retain(|name, _| !rejected.contains(name));
            rows.aliases.retain(|_, owner| !rejected.contains(owner));
        });
        catalog
            .sections
            .retain(|section| is_section(&section.key) || !rejected.contains(&section.key));
        for section in &mut catalog.sections {
            let removed_before = removed_positions
                .iter()
                .filter(|index| **index < section.index)
                .count();
            section.index -= removed_before;
        }
        (catalog, removed_errors)
    }

    /// Rows replaced by the same key's row in `bundled` when it has one, and left out otherwise
    pub fn with_rejected_replaced<'a>(
        self,
        errors: &'a [Located],
        bundled: &Catalog,
    ) -> (Self, Vec<Rejection<'a>>) {
        let (mut catalog, removed) = self.without_rejected(errors);
        let mut rejected: IndexMap<String, Vec<&'a Located>> = IndexMap::new();
        for error in removed {
            if let Some(key) = crate::error::pointer_root(&error.path) {
                rejected.entry(key).or_default().push(error);
            }
        }
        let rejections: Vec<_> = rejected
            .into_iter()
            .map(|(key, errors)| Rejection {
                replaced_by_bundled: bundled.rows().entries.contains_key(&key),
                key,
                errors,
            })
            .collect();
        catalog.store.edit(|rows| {
            for rejection in rejections
                .iter()
                .filter(|rejection| rejection.replaced_by_bundled)
            {
                rows.entries.insert(
                    rejection.key.clone(),
                    bundled.rows().entries[&rejection.key].clone(),
                );
            }
        });
        (catalog, rejections)
    }

    pub fn unrecognized(&self) -> impl Iterator<Item = (&str, &str)> {
        self.rows().entries.iter().flat_map(|(name, entry)| {
            entry
                .model
                .iter()
                .flat_map(|model| &model.unrecognized)
                .map(move |key| (name.as_str(), key.as_str()))
        })
    }

    pub fn lookup(&self, key: &str) -> Option<ModelMatch<'_>> {
        let rows = self.rows();
        let found = self.store.find_key(key)?;
        let (canonical_key, entry) = rows.entries.get_key_value(rows.owner(&found))?;
        let matched_key = rows
            .entries
            .get_key_value(found.as_str())
            .map(|(key, _)| key.as_str())
            .or_else(|| {
                rows.aliases
                    .get_key_value(found.as_str())
                    .map(|(key, _)| key.as_str())
            })?;
        Some(ModelMatch {
            matched_key,
            canonical_key,
            entry,
        })
    }

    pub fn model_count(&self) -> usize {
        self.rows().entries.len()
    }

    pub fn model_names(&self) -> impl Iterator<Item = &str> {
        self.rows().entries.keys().map(String::as_str)
    }

    pub fn aliases(&self) -> &IndexMap<String, String> {
        &self.rows().aliases
    }

    fn rows(&self) -> &CatalogRows {
        self.store.rows()
    }

    pub fn sample_spec(&self) -> Option<&Value> {
        self.sections
            .iter()
            .find(|section| section.key == SAMPLE_SPEC)
            .map(|section| &section.value)
    }

    pub fn fallback_rules(&self) -> Option<&[FallbackRule]> {
        Some(self.fallback_generalizations.as_ref()?.rules.as_slice())
    }
}

fn read_row(fields: &Fields) -> (Option<Model>, Option<Vec<String>>, Vec<Located>) {
    let wire = EntryWire::read(fields);
    let aliases = wire.attributes.aliases.clone();
    let timezone_error = unknown_timezone(&wire);
    match Model::try_from(wire) {
        Ok(model) => (Some(model), aliases, timezone_error.into_iter().collect()),
        Err(mut errors) => {
            errors.extend(timezone_error);
            (None, aliases, errors)
        }
    }
}

fn is_section(key: &str) -> bool {
    key == SAMPLE_SPEC || key == FALLBACK_GENERALIZATIONS
}

fn unknown_timezone(wire: &EntryWire) -> Option<Located> {
    let zone = wire.off_peak.as_ref()?.weekday_timezone.as_deref()?.trim();
    (!zone.is_empty() && jiff::tz::TimeZone::get(zone).is_err()).then(|| {
        Located::from_path(
            "off_peak_pricing.weekday_timezone",
            Error::UnknownTimezone {
                zone: zone.to_owned(),
            },
        )
    })
}
