"""Prevent channel or dispatch implementations from returning to the Python facade."""
from __future__ import annotations

import ast
from pathlib import Path

import pytest

PYTHON_PACKAGE = Path(__file__).resolve().parents[1] / "python" / "agentchat"
IMPORT_HOOK_FILES = {
    "models.py",
    "integrations/__init__.py",
    "channels/slack/__init__.py",
    "channels/telegram/__init__.py",
    "channels/whatsapp/__init__.py",
    "channels/ycloud/__init__.py",
}


def _body(statements):
    return [node for node in statements if not (
        isinstance(node, ast.Expr)
        and isinstance(node.value, ast.Constant)
        and isinstance(node.value.value, str)
    )]


def _surface_violations(source: str, relative_path: str) -> list[str]:
    tree = ast.parse(source)
    native_names = {
        alias.asname or alias.name
        for node in tree.body
        if isinstance(node, ast.ImportFrom) and node.module == "agentchat._native"
        for alias in node.names
    }
    parents = {child: parent for parent in ast.walk(tree) for child in ast.iter_child_nodes(parent)}
    violations = []
    for node in ast.walk(tree):
        if isinstance(node, ast.ListComp | ast.SetComp | ast.DictComp | ast.GeneratorExp):
            violations.append(f"line {node.lineno}: Python comprehension implementation")
        if isinstance(node, ast.Lambda):
            value = node.body
            empty_metadata_default = (
                relative_path == "models.py"
                and isinstance(value, ast.Call)
                and isinstance(value.func, ast.Name)
                and value.func.id == "MappingProxyType"
                and len(value.args) == 1
                and isinstance(value.args[0], ast.Dict)
                and not value.args[0].keys
            )
            if not empty_metadata_default:
                violations.append(f"line {node.lineno}: Python lambda implementation")
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            if node.func.id in {"eval", "exec", "compile", "__import__"}:
                violations.append(f"line {node.lineno}: dynamic implementation loading")
        if not isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
            continue
        owner = parents.get(node)
        protocol = isinstance(owner, ast.ClassDef) and any(
            isinstance(base, ast.Name) and base.id == "Protocol" for base in owner.bases
        )
        body = _body(node.body)
        if protocol:
            if all(isinstance(statement, ast.Pass) or (
                isinstance(statement, ast.Expr)
                and isinstance(statement.value, ast.Constant)
                and statement.value.value is Ellipsis
            ) for statement in body):
                continue
            violations.append(f"line {node.lineno}: implemented protocol method {node.name}")
            continue
        if isinstance(node, ast.AsyncFunctionDef):
            violations.append(f"line {node.lineno}: Python async implementation {node.name}")
            continue
        if relative_path in IMPORT_HOOK_FILES and node.name in {"__getattr__", "__dir__"}:
            # Lazy imports preserve optional dependency errors; they cannot execute a channel.
            if not any(isinstance(item, ast.Await | ast.For | ast.While | ast.AsyncFor)
                       for item in ast.walk(node)):
                continue
        if len(body) == 1 and isinstance(body[0], ast.Return | ast.Expr):
            call = body[0].value
            if isinstance(call, ast.Call) and isinstance(call.func, ast.Name):
                if call.func.id in native_names and all(
                    not isinstance(argument, ast.Call | ast.Await | ast.IfExp)
                    for argument in [*call.args, *(keyword.value for keyword in call.keywords)]
                ):
                    continue
        violations.append(f"line {node.lineno}: Python implementation {node.name}")
    return violations


def test_python_package_contains_types_exports_and_native_forwarders_only():
    failures = {
        str(path.relative_to(PYTHON_PACKAGE)): violations
        for path in sorted(PYTHON_PACKAGE.rglob("*.py"))
        if (violations := _surface_violations(
            path.read_text(), path.relative_to(PYTHON_PACKAGE).as_posix(),
        ))
    }
    assert not failures, failures


@pytest.mark.parametrize("source", [
    "async def run():\n    await client.run()\n",
    "def normalize(value):\n    return value['message']\n",
    ("from typing import Protocol\nclass Channel(Protocol):\n"
     "    async def run(self):\n        await client.run()\n"),
    "exec('def run(): pass')\n",
    "normalize = lambda value: value['message']\n",
])
def test_surface_guard_rejects_python_business_logic(source):
    assert _surface_violations(source, "channels/example/adapter.py")


def test_surface_guard_accepts_types_and_direct_native_forwarding():
    source = """
from typing import Protocol
from agentchat._native import initialize_error
class Client(Protocol):
    async def close(self): ...
class NativeError(RuntimeError):
    def __init__(self, description):
        initialize_error(self, description)
"""
    assert not _surface_violations(source, "channels/example/transport.py")
