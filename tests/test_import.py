import gtharness


def test_native_module_is_built():
    assert gtharness._native.__name__ == "gtharness._native"
