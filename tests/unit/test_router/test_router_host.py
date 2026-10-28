import subprocess
import sys

import pytest

import litellm.router_host as router_host_module
from litellm.router_host import RouterHost, router_host


def test_router_uses_the_proxy_host_without_importing_the_proxy_first():
    # A fresh interpreter, so no earlier test has imported litellm.proxy.
    script = (
        "import sys\n"
        "from litellm.router import Router\n"
        "from litellm.router_host import router_host\n"
        "assert 'litellm.proxy' not in sys.modules, 'importing the router imported the proxy'\n"
        "print(type(router_host()).__name__)\n"
    )
    result = subprocess.run([sys.executable, "-P", "-c", script], capture_output=True, text=True, check=True)
    assert result.stdout.strip() == "ProxyRouterHost"


@pytest.fixture(autouse=True)
def nothing_installed():
    saved = router_host_module._installed[0]
    router_host_module._installed[0] = None
    yield
    router_host_module._installed[0] = saved


def test_router_falls_back_to_the_default_host_when_the_proxy_cannot_be_imported(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setitem(sys.modules, "litellm.proxy._router_host", None)

    assert type(router_host()) is RouterHost


def test_router_uses_the_proxy_host_when_the_proxy_was_imported_before():
    from litellm.proxy._router_host import ProxyRouterHost

    assert type(router_host()) is ProxyRouterHost


def test_an_installed_host_is_kept():
    host = RouterHost()
    router_host_module.install_router_host(host)

    assert router_host() is host
