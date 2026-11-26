"""gth_core::Error variants arrive in Python as the gtharness.errors hierarchy."""

import pickle

import pytest

from gtharness import _native
from gtharness.errors import CoreLoadError, DesyncError, HarnessError


def test_hierarchy():
    assert issubclass(HarnessError, Exception)
    assert issubclass(CoreLoadError, HarnessError)
    assert issubclass(DesyncError, HarnessError)
    assert not issubclass(CoreLoadError, DesyncError)
    for cls in (HarnessError, CoreLoadError, DesyncError):
        assert cls.__module__ == "gtharness.errors"


@pytest.mark.parametrize(
    ("kind", "cls", "message"),
    [
        ("core_load", CoreLoadError, "cannot load core missing.so: test"),
        ("desync", DesyncError, "replay desynced at frame 7: test"),
    ],
)
def test_core_errors_map_to_their_exception(kind, cls, message):
    with pytest.raises(cls) as raised:
        _native._raise_core_error(kind)
    assert type(raised.value) is cls
    assert str(raised.value) == message


def test_python_errors_pass_through():
    with pytest.raises(ValueError, match="unknown error kind"):
        _native._raise_core_error("nope")


def test_exceptions_pickle():
    # Parallel environments are separate processes, so errors cross process boundaries.
    with pytest.raises(DesyncError) as raised:
        _native._raise_core_error("desync")
    copy = pickle.loads(pickle.dumps(raised.value))
    assert type(copy) is DesyncError
    assert str(copy) == str(raised.value)
