# Options

Plugins can declare typed options that users pass via `buf.gen.yaml`, giving them a structured way to configure code generation behavior.

## Plugin options

Define plugin options as a dataclass and pass it as the third argument to `run()`:

```python title="protoc-gen-hello"
from dataclasses import dataclass
from protobuf.plugin import Schema, run


@dataclass
class Options:
    verbose: bool = False


def generate(schema: Schema[Options]) -> None:
    if schema.options.verbose:
        ...


run("protoc-gen-hello", "0.1.0", Options, generate)
```

Pass options in `buf.gen.yaml` as a comma-separated `opt:` value:

```yaml title="buf.gen.yaml"
plugins:
  - local: protoc-gen-hello
    out: src/gen
    opt: verbose=true
```

Supported field types: `str`, `bool`, `int`, `float`, `Literal[...]`, `StrEnum`, `IntEnum`, `list[T]`, `dict[str, T]`.
`| None` can be added for optional options.

## Framework-reserved options

The framework reserves certain option names for all plugins, currently: `no_fmt_off`, `escape_module_with_hash`, and `rewrite_imports`.
If your `Options` dataclass defines a field with a reserved name, `run()` raises a `ValueError`.

### no_fmt_off

When set, omits the `# fmt: off` line from the generated file preamble.
Use this when you want ruff to format the generated output:

```yaml title="buf.gen.yaml"
plugins:
  - local: protoc-gen-hello
    out: src/gen
    opt: no_fmt_off
```

### escape_module_with_hash

Module names are derived from proto file paths, and characters that aren't valid in a Python module name (such as `.` or `-`) are replaced with underscores.
That replacement can cause separate proto files to map to the same module name.
When set, this option appends a short hash suffix, derived from the original unsanitized name, to avoid those collisions:

```yaml title="buf.gen.yaml"
plugins:
  - local: protoc-gen-hello
    out: src/gen
    opt: escape_module_with_hash
```

### rewrite_imports

Rewrites imports of generated modules that match a glob pattern to an absolute package.
This makes it possible to reference generated code published from a separate package.

The option takes the form `rewrite_imports=<pattern>:<target>` and can be given multiple times; the first matching pattern wins.
The pattern is a very reduced subset of glob:

- `*` matches zero or more characters except `/`.
- `**/` matches zero or more path elements, where an element is one or more characters with a trailing `/`.

The pattern is matched against the import path of the module before it is made relative to the file importing it.
A generated module such as `.google.type.foo_pb` is matched as the file path `./google/type/foo_pb.py`, relative to the generation root.
On a match, the target package is prepended:

```yaml title="buf.gen.yaml"
plugins:
  - local: protoc-gen-hello
    out: src/gen
    opt: rewrite_imports=./google/type/**/*_pb.py:mypkg.gen
```

With this option, `from .google.type.foo_pb import Foo` is generated as `from mypkg.gen.google.type.foo_pb import Foo` instead.
References to symbols defined in the file being generated are not imports and are never rewritten.

An empty target rewrites matching imports to the canonical import path, the module path derived from the proto file name, relative to the root of `sys.path`.
For example, when `buf/validate/validate.proto` is provided by an installed package:

```yaml title="buf.gen.yaml"
plugins:
  - local: protoc-gen-hello
    out: src/gen
    opt: rewrite_imports=./buf/validate/**/*_pb.py:
```

This generates `from buf.validate import validate_pb` instead of a relative import that points at a location that does not exist in the output directory.
A rewritten module import that lands at the top level (for example a proto file at the root of the module) is written as a plain `import foo_pb` statement.

Absolute imports (such as `protobuf.wkt`) are matched without a leading `./` and `.py` extension (for example `protobuf/wkt`), and are replaced by the target entirely.

## Example: Sensitive fields plugin

Here is a plugin that generates a `_sensitive.py` file for each proto file, listing all fields marked with the `sensitive` custom option from [extensions](../extensions.md#extensions-in-custom-options):

```python title="protoc-gen-sensitive"
#!/usr/bin/env python3
from protobuf.plugin import Schema, run
from gen.options_pb import ext_sensitive


def generate(schema: Schema) -> None:
    for desc in schema.files_to_generate:
        f = schema.generate_file(desc, "_sensitive.py")
        f.preamble(desc)
        f.print()

        sensitive_fields = [
            (msg, field)
            for msg in desc.messages
            for field in msg.fields
            if field.proto.options is not None and field.proto.options[ext_sensitive]
        ]

        with f.scope("SENSITIVE_FIELDS: list[tuple[str, str]] = ["):
            for msg, field in sensitive_fields:
                f.print(f'("{msg.name}", "{field.name}"),')
        f.print("]")


run("protoc-gen-sensitive", "0.1.0", generate)
```
