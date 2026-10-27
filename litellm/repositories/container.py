from dataclasses import dataclass

from litellm.repositories.deployment_warning_ignore_repository import DeploymentWarningIgnoreRepository
from litellm.repositories.user_banner_repository import UserBannerRepository


@dataclass(frozen=True, slots=True)
class Repositories:
    banners: UserBannerRepository
    deployment_warning_ignores: DeploymentWarningIgnoreRepository
