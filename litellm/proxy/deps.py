from typing import Annotated, Final, TypeAlias

from fastapi import Depends, Request

from litellm.repositories.container import Repositories
from litellm.repositories.database import Database
from litellm.repositories.deployment_warning_ignore_repository import DeploymentWarningIgnoreRepository
from litellm.repositories.user_banner_repository import UserBannerRepository

_MISSING_DATABASE: Final = object()


class DatabaseStateMissingError(RuntimeError):
    def __init__(self) -> None:
        super().__init__("The proxy lifespan did not publish a Database in request state")


async def get_database(request: Request) -> Database | None:
    database: Final[object] = getattr(request.state, "database", _MISSING_DATABASE)
    if database is None or isinstance(database, Database):
        return database
    raise DatabaseStateMissingError()


DatabaseDependency: TypeAlias = Annotated[Database | None, Depends(get_database)]


async def get_repositories(database: DatabaseDependency) -> Repositories | None:
    if database is None:
        return None
    return Repositories(
        banners=UserBannerRepository(database.prisma),
        deployment_warning_ignores=DeploymentWarningIgnoreRepository(database.prisma, use_writer=True),
    )


RepositoriesDependency: TypeAlias = Annotated[Repositories | None, Depends(get_repositories)]
