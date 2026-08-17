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


from __future__ import annotations

import re

RewriteImports = tuple[tuple[re.Pattern[str], str], ...]

_OPTION_NAME = "rewrite_imports"


def compile_rewrite_imports(rewrites: dict[str, str]) -> RewriteImports:
    return tuple(
        (_glob_to_regex(pattern), target.rstrip("."))
        for pattern, target in rewrites.items()
    )


def rewrite_module_path(module_path: str, rewrites: RewriteImports) -> str | None:
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
