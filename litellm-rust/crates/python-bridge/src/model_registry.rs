use pyo3::prelude::*;
use pyo3::types::PyBytes;

#[pyfunction]
pub fn bundled_model_cost_map(py: Python<'_>) -> Bound<'_, PyBytes> {
    PyBytes::new(py, litellm_model_catalog::BUNDLED_REGISTRY)
}
