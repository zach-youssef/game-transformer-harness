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


def parameters(node: ast.FunctionDef) -> list[str]:
    return [a.arg for a in node.args.posonlyargs + node.args.args]


def test_stub_parameters_match_module():
    for name, node in stub_definitions().items():
        if isinstance(node, ast.FunctionDef):
            native_params = list(inspect.signature(getattr(_native, name)).parameters)
            assert parameters(node) == native_params, name


def test_stub_classes_match_module():
    """Classes with members (not the exceptions): the same public members and parameters."""
    for name, node in stub_definitions().items():
        members = [m for m in getattr(node, "body", []) if isinstance(m, ast.FunctionDef)]
        if not members:
            continue
        cls = getattr(_native, name)
        stub = {m.name: m for m in members if m.name != "__new__"}
        native = {n for n in dir(cls) if not n.startswith("_")}
        assert set(stub) == native, name
        for member, method in stub.items():
            if any(ast.unparse(d) == "property" for d in method.decorator_list):
                assert inspect.isdatadescriptor(getattr(cls, member)), f"{name}.{member}"
                continue
            native_params = list(inspect.signature(getattr(cls, member)).parameters)
            assert parameters(method) == native_params, f"{name}.{member}"
        new = next((m for m in members if m.name == "__new__"), None)
        if new is not None:
            assert parameters(new)[1:] == list(inspect.signature(cls).parameters), name
