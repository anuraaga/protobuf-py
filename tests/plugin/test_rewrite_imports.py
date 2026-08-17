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

from textwrap import dedent
from typing import TYPE_CHECKING

import pytest

from protobuf.plugin import Ident, Module
from protobuf.plugin._file import _File, write as gen_write
from protobuf.plugin._rewrite_imports import (
    compile_rewrite_imports,
    rewrite_module_path,
)

if TYPE_CHECKING:
    from protobuf import DescFile
    from tests.conftest import Protoc


class TestRewriteModulePath:
    @pytest.mark.parametrize(
        ("pattern", "module_path", "expected"),
        [
            pytest.param("./foo/*_pb.py", ".foo.bar_pb", "pkg.foo.bar_pb", id="star"),
            pytest.param(
                "./foo/*_pb.py",
                ".foo.baz.bar_pb",
                None,
                id="star_does_not_cross_separator",
            ),
            pytest.param(
                "./foo/**/*_pb.py",
                ".foo.baz.qux.bar_pb",
                "pkg.foo.baz.qux.bar_pb",
                id="globstar",
            ),
            pytest.param(
                "./foo/**/*_pb.py",
                ".foo.bar_pb",
                "pkg.foo.bar_pb",
                id="globstar_matches_zero_elements",
            ),
            pytest.param("./**/*_pb.py", ".bar_pb", "pkg.bar_pb", id="root_globstar"),
            pytest.param("./foo/*_pb.py", ".foo.bar_px", None, id="suffix_mismatch"),
            pytest.param("./bar/*_pb.py", ".foo.bar_pb", None, id="prefix_mismatch"),
            pytest.param("foo/*_pb.py", ".foo.bar_pb", None, id="missing_leading_dot"),
            pytest.param("./b.r_pb.py", ".bar_pb", None, id="dot_is_literal"),
            pytest.param(
                "protobuf/wkt", "protobuf.wkt", "pkg", id="absolute_replaced_entirely"
            ),
            pytest.param("protobuf/*", "protobuf.wkt", "pkg", id="absolute_star"),
            pytest.param(
                "./protobuf/wkt.py", "protobuf.wkt", None, id="absolute_not_relative"
            ),
        ],
    )
    def test_single_pattern(
        self, pattern: str, module_path: str, expected: str | None
    ) -> None:
        rewrites = compile_rewrite_imports({pattern: "pkg"})
        assert rewrite_module_path(module_path, rewrites) == expected

    def test_first_match_wins(self) -> None:
        rewrites = compile_rewrite_imports(
            {"./foo/*_pb.py": "first", "./**/*_pb.py": "second"}
        )
        assert rewrite_module_path(".foo.bar_pb", rewrites) == "first.foo.bar_pb"
        assert rewrite_module_path(".other.bar_pb", rewrites) == "second.other.bar_pb"

    def test_target_trailing_dot_stripped(self) -> None:
        rewrites = compile_rewrite_imports({"./*_pb.py": "pkg."})
        assert rewrite_module_path(".bar_pb", rewrites) == "pkg.bar_pb"

    @pytest.mark.parametrize("target", ["", "."])
    def test_empty_target_is_canonical(self, target: str) -> None:
        rewrites = compile_rewrite_imports({"./**/*_pb.py": target})
        assert rewrite_module_path(".foo.bar_pb", rewrites) == "foo.bar_pb"
        assert rewrite_module_path(".bar_pb", rewrites) == "bar_pb"

    def test_empty_target_absolute_raises(self) -> None:
        rewrites = compile_rewrite_imports({"protobuf/*": ""})
        with pytest.raises(ValueError, match="rewrite_imports"):
            rewrite_module_path("protobuf.wkt", rewrites)


class TestFileRewrites:
    def test_symbol_import(self, desc: DescFile) -> None:
        f = _file(desc, {"./dep_pb.py": "mypkg.gen"})
        f.print("x: ", desc.dependencies[0].messages[0])
        assert gen_write(f, f.path) == dedent(
            """\
            from __future__ import annotations

            from mypkg.gen.dep_pb import Dep


            x: Dep
            """
        )

    def test_module_import(self, desc: DescFile) -> None:
        f = _file(desc, {"./pkg/**/*_pb.py": "mypkg.gen"})
        f.print("d = ", desc.dependencies[1], ".desc()")
        assert gen_write(f, f.path) == dedent(
            """\
            from __future__ import annotations

            from mypkg.gen.pkg import nested_pb


            d = nested_pb.desc()
            """
        )

    def test_own_symbols_not_rewritten(self, desc: DescFile) -> None:
        f = _file(desc, {"./**/*_pb.py": "mypkg.gen"})
        f.print("x: ", desc.messages[0])
        f.print("y: ", desc.dependencies[0].messages[0])
        assert gen_write(f, f.path) == dedent(
            """\
            from __future__ import annotations

            from mypkg.gen.dep_pb import Dep


            x: Foo
            y: Dep
            """
        )

    def test_unmatched_import_stays_relative(self, desc: DescFile) -> None:
        f = _file(desc, {"./pkg/**/*_pb.py": "mypkg.gen"})
        f.print("x: ", desc.dependencies[0].messages[0])
        assert gen_write(f, f.path) == dedent(
            """\
            from __future__ import annotations

            from .dep_pb import Dep


            x: Dep
            """
        )

    def test_type_only_import(self, desc: DescFile) -> None:
        f = _file(desc, {"./dep_pb.py": "mypkg.gen"})
        f.print("x: ", Ident.for_desc(desc.dependencies[0].messages[0], type_only=True))
        assert gen_write(f, f.path) == dedent(
            """\
            from __future__ import annotations

            from typing import TYPE_CHECKING

            if TYPE_CHECKING:
                from mypkg.gen.dep_pb import Dep


            x: Dep
            """
        )

    def test_absolute_import_replaced(self, desc: DescFile) -> None:
        f = _file(desc, {"protobuf/wkt": "vendored.wkt"})
        f.print("x: ", Module("protobuf.wkt").ident("Timestamp"))
        assert gen_write(f, f.path) == dedent(
            """\
            from __future__ import annotations

            from vendored.wkt import Timestamp


            x: Timestamp
            """
        )

    def test_canonical_external_dependency(self, protoc: Protoc) -> None:
        """An import of a dependency provided by a separate package.

        Ported from https://github.com/bufbuild/protobuf-py/pull/15: the
        dependency is not generated alongside the importing file, so its
        relative import would be broken. rewrite_imports with an empty
        target produces the canonical import instead.
        """
        files = protoc.compile(
            {
                "app/main.proto": """
                syntax = "proto3";
                package app;
                import "buf/validate/validate.proto";
                message Main {
                    buf.validate.Rule rule = 1;
                }
                """,
                "buf/validate/validate.proto": """
                syntax = "proto3";
                package buf.validate;
                message Rule {}
                """,
            },
            "include_imports",
        )
        dep = files["buf/validate/validate.proto"]
        f = _file(files["app/main.proto"], {"./buf/validate/**/*_pb.py": ""})
        f.print(dep, ".desc()")
        f.print("rule: ", dep.messages[0])
        assert gen_write(f, f.path) == dedent(
            """\
            from __future__ import annotations

            from buf.validate import validate_pb
            from buf.validate.validate_pb import Rule


            validate_pb.desc()
            rule: Rule
            """
        )

    def test_canonical_root_external_dependency(self, protoc: Protoc) -> None:
        """A root-level external dependency becomes a plain `import X`."""
        files = protoc.compile(
            {
                "main.proto": """
                syntax = "proto3";
                import "dep.proto";
                message Main {
                    Dep dep = 1;
                }
                """,
                "dep.proto": """
                syntax = "proto3";
                message Dep {}
                """,
            },
            "include_imports",
        )
        dep = files["dep.proto"]
        f = _file(files["main.proto"], {"./dep_pb.py": ""})
        f.print(dep, ".desc()")
        f.print("dep: ", dep.messages[0])
        assert gen_write(f, f.path) == dedent(
            """\
            from __future__ import annotations

            import dep_pb
            from dep_pb import Dep


            dep_pb.desc()
            dep: Dep
            """
        )

    @pytest.fixture
    def desc(self, protoc: Protoc) -> DescFile:
        return protoc.compile(
            {
                "input.proto": """
                syntax = "proto3";
                import "dep.proto";
                import "pkg/nested.proto";
                message Foo {
                    Dep dep = 1;
                    pkg.Nested nested = 2;
                }
            """,
                "dep.proto": 'syntax = "proto3"; message Dep {}',
                "pkg/nested.proto": 'syntax = "proto3"; package pkg; message Nested {}',
            },
            "include_imports",
        )["input.proto"]


def _file(desc: DescFile, rewrites: dict[str, str]) -> _File:
    module = Module.for_desc(desc, "_pb")
    return _File(
        path=f"{module.path.removeprefix('.').replace('.', '/')}.py",
        module=module,
        file_to_generate=frozenset(),
        plugin_name="test",
        plugin_version="0.0.0",
        parameter="",
        rewrite_imports=compile_rewrite_imports(rewrites),
    )
