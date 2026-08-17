# Copyright (c) 2025-2026 Buf Technologies, Inc.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Import path rewriting for the `rewrite_imports` framework option.

Mirrors the `rewrite_imports` option of protobuf-es plugins, which makes
it possible to serve generated code for a module and its dependencies as
separate packages. The option can be given multiple times, in the form
`rewrite_imports=<pattern>:<target>`.

The pattern is a very reduced subset of glob:

- `*` matches zero or more characters except `/`.
- `**/` matches zero or more path elements, where an element is one or
  more characters with a trailing `/`.

The target is an absolute Python package path, for example `mypkg.gen`.
An empty target (or `.`) rewrites the import to the canonical import
path: the module path derived from the proto file name, relative to the
root of `sys.path`. This is the layout used by generated SDKs published
as standalone packages.

If any generated file imports from a module matching one of the
patterns, the import is rewritten to the corresponding target. The
pattern is matched against the import path of the module before it is
made relative to the file importing it, and the first matching pattern
wins.
"""

from __future__ import annotations

import re

RewriteImports = tuple[tuple[re.Pattern[str], str], ...]
"""Compiled `rewrite_imports` entries as (pattern, target) pairs."""

_OPTION_NAME = "rewrite_imports"


def compile_rewrite_imports(rewrites: dict[str, str]) -> RewriteImports:
    """Compile raw `pattern -> target` entries into matchable form.

    Args:
        rewrites: Parsed `rewrite_imports` entries, keyed by pattern.

    Returns:
        Compiled (pattern, target) pairs in the original order. An
        empty target denotes the canonical import path.
    """
    return tuple(
        (_glob_to_regex(pattern), target.rstrip("."))
        for pattern, target in rewrites.items()
    )


def rewrite_module_path(module_path: str, rewrites: RewriteImports) -> str | None:
    """Apply the first matching rewrite to a module path.

    The module path is matched as an import path: a module relative to
    the generation root such as `.google.type.foo_pb` is matched as
    `./google/type/foo_pb.py`, and an absolute module such as
    `protobuf.wkt` is matched as `protobuf/wkt`. On a match, a relative
    module is rewritten by prepending the target, while an absolute
    module is replaced by the target entirely. An empty target rewrites
    a relative module to its canonical import path.

    Args:
        module_path: The dotted module path being imported.
        rewrites: Compiled (pattern, target) pairs.

    Returns:
        The rewritten absolute module path, or None if no pattern
        matches.

    Raises:
        ValueError: If an absolute module matches a pattern with an
            empty target.
    """
    relative = module_path.startswith(".")
    segments = module_path.removeprefix(".").split(".")
    if not all(segments):
        return None
    import_path = f"./{'/'.join(segments)}.py" if relative else "/".join(segments)
    for pattern, target in rewrites:
        if pattern.fullmatch(import_path):
            if relative:
                dotted = ".".join(segments)
                return f"{target}.{dotted}" if target else dotted
            if not target:
                msg = f"option '{_OPTION_NAME}': cannot rewrite absolute import '{module_path}' to an empty target"
                raise ValueError(msg)
            return target
    return None


def _glob_to_regex(pattern: str) -> re.Pattern[str]:
    """Translate a reduced glob pattern into a regular expression."""
    parts: list[str] = []
    i = 0
    while i < len(pattern):
        char = pattern[i]
        if char == "*":
            if pattern[i + 1 : i + 3] == "*/":
                parts.append(r"([^/]+/)*")
                i += 3
                continue
            parts.append(r"[^/]*")
        else:
            parts.append(re.escape(char))
        i += 1
    return re.compile("".join(parts))
