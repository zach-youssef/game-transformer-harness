"""The type stubs in _native.pyi list exactly what the extension module exports."""

import ast
import inspect
from pathlib import Path

import gtharness
from gtharness import _native

STUB = Path(gtharness.__file__).with_name("_native.pyi")


def stub_definitions() -> dict[str, ast.stmt]:
    tree = ast.parse(STUB.read_text(encoding="utf-8"))
    defs: dict[str, ast.stmt] = {}
    for node in tree.body:
        if isinstance(node, ast.FunctionDef | ast.ClassDef):
            defs[node.name] = node
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            defs[node.target.id] = node
    return defs


def module_names() -> set[str]:
    return {n for n in dir(_native) if not n.startswith("__") or n == "__version__"}


def test_stub_names_match_module():
    assert set(stub_definitions()) == module_names()


def test_stub_parameters_match_module():
    for name, node in stub_definitions().items():
        if isinstance(node, ast.FunctionDef):
            stub_params = [a.arg for a in node.args.posonlyargs + node.args.args]
            native_params = list(inspect.signature(getattr(_native, name)).parameters)
            assert stub_params == native_params, name
