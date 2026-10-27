from dataclasses import dataclass

from litellm.repositories.user_banner_repository import UserBannerRepository


@dataclass(frozen=True, slots=True)
class Repositories:
    banners: UserBannerRepository
