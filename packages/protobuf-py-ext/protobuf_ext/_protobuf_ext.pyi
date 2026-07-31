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

from typing import Generic, TypeVar, final

from protobuf import Message
from protobuf._registry import Registry
from protobuf._typing import JsonValue

class NativeMessage:
    def to_binary(self, *, write_unknown_fields: bool = True) -> bytes: ...
    def to_json(
        self,
        *,
        registry: Registry | None = None,
        always_emit_implicit: bool = False,
        print_enums_as_ints: bool = False,
        use_proto_field_name: bool = False,
    ) -> str: ...
    def _merge_from_binary(
        self, data: bytes, ignore_unknown_fields: bool = False
    ) -> None: ...
    def _merge_from_json(
        self,
        json: str | bytes | bytearray,
        *,
        ignore_unknown_fields: bool = False,
        registry: Registry | None = None,
    ) -> None: ...
    def _to_json_value(
        self,
        *,
        registry: Registry | None = None,
        always_emit_implicit: bool = False,
        print_enums_as_ints: bool = False,
        use_proto_field_name: bool = False,
    ) -> JsonValue: ...
    @classmethod
    def _from_json_value(
        cls,
        data: JsonValue,
        *,
        ignore_unknown_fields: bool = False,
        registry: Registry | None = None,
    ) -> Message: ...

_C = TypeVar("_C", bound=str)  # Case name
_V = TypeVar("_V")  # Value type

@final
class Oneof(Generic[_C, _V]):
    def __init__(self, field: _C, value: _V) -> None: ...
    field: _C
    value: _V

def initialize_message_type(message_type: type[Message]) -> None: ...
def generic_setattr(obj: object, name: str, value: object) -> None: ...
