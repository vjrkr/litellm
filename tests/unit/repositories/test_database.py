import asyncio
from typing import Final
from unittest.mock import AsyncMock, MagicMock

import pytest

from litellm.repositories.database import Database, lifespan


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", [RuntimeError, asyncio.CancelledError])
async def test_lifespan_closes_database_when_owner_fails(failure):
    prisma: Final = MagicMock()
    prisma.stop_background_tasks = AsyncMock()
    prisma.disconnect = AsyncMock()
    database: Final = Database(prisma=prisma)

    async def fail():
        async with lifespan(database) as current:
            assert current is database
            prisma.disconnect.assert_not_awaited()
            raise failure("owner failed")

    with pytest.raises(failure):
        await fail()
    prisma.disconnect.assert_awaited_once()


@pytest.mark.asyncio
async def test_lifespan_without_database_preserves_owner_error():
    async def fail():
        async with lifespan(None) as database:
            assert database is None
            raise RuntimeError("owner failed")

    with pytest.raises(RuntimeError, match="owner failed"):
        await fail()


@pytest.mark.asyncio
async def test_cancelling_owner_waits_for_database_close() -> None:
    serving: Final = asyncio.Event()
    closing: Final = asyncio.Event()
    release_close: Final = asyncio.Event()
    prisma: Final = MagicMock()
    prisma.stop_background_tasks = AsyncMock()

    async def disconnect() -> None:
        closing.set()
        await release_close.wait()

    prisma.disconnect = AsyncMock(side_effect=disconnect)

    async def serve() -> None:
        async with lifespan(Database(prisma=prisma)):
            serving.set()
            await asyncio.Event().wait()

    owner: Final = asyncio.create_task(serve())
    try:
        await asyncio.wait_for(serving.wait(), timeout=2)
        owner.cancel()
        await asyncio.wait_for(closing.wait(), timeout=2)
        assert not owner.done()
        release_close.set()
        with pytest.raises(asyncio.CancelledError):
            await owner
    finally:
        release_close.set()
        if not owner.done():
            owner.cancel()
        await asyncio.gather(owner, return_exceptions=True)
    prisma.disconnect.assert_awaited_once()


@pytest.mark.asyncio
async def test_database_disconnects_even_when_stopping_background_tasks_fails() -> None:
    prisma: Final = MagicMock()
    prisma.stop_background_tasks = AsyncMock(side_effect=RuntimeError("watchdog failed"))
    prisma.disconnect = AsyncMock()

    with pytest.raises(RuntimeError, match="watchdog failed"):
        async with lifespan(Database(prisma=prisma)):
            pass

    prisma.disconnect.assert_awaited_once()
