# Python and FastAPI

## Existing pattern

- `get_database` reads the lifespan-owned handle from request state
- `get_repositories` builds the shared access point from that handle
- A handler receives `Repositories | None`, then uses `repositories.banners`, never a Prisma client
- `UserBannerRepository` keeps the existing Prisma table, serialization and configuration-sync behavior
- The banner audit provider binds the legacy audit callable at request time, without performing audit I/O
- SQLx can later implement the same typed operations without changing handler signatures

## Dependency injection

- Group operations by domain in `Repositories`, adding fields as callers migrate
- The provider builds the container from existing handles

```python
@dataclass(frozen=True, slots=True)
class Repositories:
    banners: UserBannerRepository


async def get_repositories(database: DatabaseDependency) -> Repositories | None:
    return None if database is None else Repositories(banners=UserBannerRepository(database.prisma))


RepositoriesDependency: TypeAlias = Annotated[Repositories | None, Depends(get_repositories)]
```

- FastAPI supplies the container to routes and caches the provider within each request
- Ordinary helpers and jobs receive explicit arguments, never unresolved `Depends` defaults

```python
async def get_user_banner(repositories: RepositoriesDependency) -> UserBanner:
    if repositories is None:
        return UserBanner()
    return await repositories.banners.get_banner()
```

## Lifespan state

- Prisma startup owns connection setup, authentication refresh, health checks and migrations
- `Database` wraps the initialized Prisma client, without opening another pool or holding fields per feature
- FastAPI lifespan enters `litellm.repositories.database.lifespan` and publishes its handle in request state
- Starlette shallow-copies lifespan state into request state, which is distinct from `app.state`
- The database module never imports a repository, legacy globals borrow its handle without owning shutdown
- Drain requests, jobs, logging, spend and audit work before database teardown
- `litellm.repositories.database.lifespan` closes the handle on exit, including failures after entry
- When adding native resources, initialize them in the serving process, respecting runtime fork restrictions
- Await native shutdown instead of depending on Python garbage collection
- Apply the [connection ownership rules](../SKILL.md#connection-ownership) when changing the owner

## Incremental migration

- Port one operation while other operations stay on Prisma, without waiting for unrelated routes to adopt dependency injection
- Keep callers using `repositories.banners.get_banner()` with the same inputs and outputs
- This example ports banner reads while writes remain Prisma
  - `NativeBannerAdapter` and `database.native` are proposed interfaces, not implemented by the current banner PR
  - The adapter converts typed PyO3 results to `UserBanner`, preserving defaults, invalid-data handling and storage errors
  - SQL stays in `crates/db/queries/`, Python never sends SQL strings

```diff
 class UserBannerRepository(PrismaTableRepository["prisma_models.LiteLLM_UISettings"]):
     table_name = "litellm_uisettings"

+    def __init__(self, prisma: PrismaClient, native: NativeBannerAdapter) -> None:
+        super().__init__(prisma)
+        self._native = native
+
     async def get_banner(self) -> UserBanner:
-        return parse_user_banner(await self.get_raw_settings())
+        return await self._native.get_banner()

-    async def get_raw_settings(self) -> object:
-        db_record: Final = await self.table.find_unique(where={"id": USER_BANNER_ROW_ID})
-        return db_record.ui_settings if db_record is not None else None
```

- Keep `save_banner` and its Prisma write unchanged
- Preserve existing inheritance only while unmigrated methods need it, new repositories use composition
- After the existing lifecycle owner initializes the native handle, update the provider

```diff
 async def get_repositories(database: DatabaseDependency) -> Repositories | None:
-    return None if database is None else Repositories(banners=UserBannerRepository(database.prisma))
+    if database is None:
+        return None
+    return Repositories(
+        banners=UserBannerRepository(prisma=database.prisma, native=database.native),
+    )
```

- Both handles coexist for different operations, only the native `get_banner` implementation remains
- Switch every caller, including helpers and jobs, and delete the old query in the same PR
- Validate against real PostgreSQL and refresh SQLx metadata
- Move a complete transaction together when the query belongs to one
- Remove the Prisma handle and inherited table access after the repository's last Prisma operation moves
- Roll back the release if needed, never retry an uncertain native write through Prisma

## Timing and errors

- FastAPI resolves dependencies before entering the handler
- Handler exception handling cannot catch failures raised during dependency resolution
- A universal required-database dependency can override authorization and validation behavior
- Banner GET without a database returns the default disabled banner
- Banner PATCH checks the admin role before returning the existing missing-database error
- Invalid stored banner data falls back to the default, but storage failures still propagate
- Banner writes finish before the success audit is queued as a FastAPI background task
- Queue post-response work with `BackgroundTasks`, not bare `asyncio.create_task`, so the in-flight drain waits for it before the database closes
- Do not move background work into an awaited provider merely to remove an import

## Verification

- Exercise real FastAPI handlers using `dependency_overrides` for external boundaries
- Keep production parsing and persistence code real
- Restore overrides after each test
- Verify complete response bodies, status codes and OpenAPI against the base
- Include combined failure conditions to pin error precedence
- Check saved values, fresh revisions, audit contents and failed-write behavior
- Check lifespan state with a running lifespan, not only a TestClient that skips startup
- Verify providers do not connect, query, disconnect or audit eagerly
- Use real PostgreSQL and live proxy requests when changing persistence behavior

## Scope

- Preserve direct Python call sites or migrate them explicitly
- Inventory imports, state writers, side effects and error ordering before migrating a feature
- Preserve configuration sync and serialization, and state which legacy reads remain
- Do not migrate every route in a large module just to establish the pattern
- Do not invent providers for resources without consumers

## Framework references

- [Dependency composition and caching](https://fastapi.tiangolo.com/tutorial/dependencies/sub-dependencies/)
- [Dependency overrides](https://fastapi.tiangolo.com/advanced/testing-dependencies/)
