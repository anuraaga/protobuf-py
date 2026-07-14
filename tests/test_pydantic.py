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

import pytest

pytest.importorskip("pydantic", reason="optional dependency not installed")

from typing import TYPE_CHECKING

from pydantic import BaseModel, TypeAdapter, ValidationError

from protobuf.wkt import Duration, FileDescriptorSet, Timestamp

from .gen.messages_pb import MixedFields
from .gen.scalars_pb import Scalars
from .gen.wkt_pb import WellKnownTypes

if TYPE_CHECKING:
    from protobuf import Message


@pytest.mark.parametrize(
    "msg",
    [
        pytest.param(Scalars(), id="empty"),
        pytest.param(
            Scalars(uint32_field=124, bool_field=True, float_field=3.0), id="scalars"
        ),
        pytest.param(
            WellKnownTypes(
                duration=Duration.from_seconds(60),
                timestamp=Timestamp.from_seconds(3600),
            ),
            id="wkts",
        ),
    ],
)
def test_adapter(msg: Message) -> None:
    ta = TypeAdapter(msg.__class__)

    assert ta.dump_python(msg) == msg
    assert ta.dump_json(msg) == msg.to_json().encode()
    assert ta.validate_python(msg) == msg
    assert ta.validate_json(msg.to_json().encode()) == msg


def test_adapter_invalid() -> None:
    ta = TypeAdapter(Scalars)
    with pytest.raises(ValidationError):
        ta.validate_python({"uint32Field": "bear"})
    with pytest.raises(ValidationError):
        assert ta.validate_json(b'{"uint32Field": "bear"}') == Scalars()


class Model(BaseModel):
    msg: Scalars


def test_model() -> None:
    s = Scalars(uint32_field=124, bool_field=True, float_field=3.0)
    m = Model(msg=s)
    assert m.msg == s
    assert m.model_dump() == {"msg": s}
    assert m.model_dump_json() == f'{{"msg":{s.to_json()}}}'

    assert Model.model_validate({"msg": s}) == m
    assert Model.model_validate_json(f'{{"msg":{s.to_json()}}}') == m


def test_model_invalid() -> None:
    with pytest.raises(ValidationError):
        Model.model_validate({"msg": {"uint32Field": "bear"}})
    with pytest.raises(ValidationError):
        Model.model_validate_json('{"msg": {"uint32Field": "bear"}}')


def test_json_schema_nested_refs() -> None:
    schema = TypeAdapter(MixedFields).json_schema()
    assert schema["properties"]["messageField"]["$ref"] == "#/$defs/Bar"
    assert schema["properties"]["explicitEnumField"]["$ref"] == "#/$defs/E"
    enum_def = schema["$defs"]["E"]
    assert enum_def["type"] == "string"
    assert enum_def["enum"] == ["E_UNSPECIFIED", "ONE", "TWO"]


def test_json_schema_field_titles() -> None:
    props = TypeAdapter(MixedFields).json_schema()["properties"]
    assert props["explicitField"]["title"] == "explicit_field"
    assert props["repeatedField"]["title"] == "repeated_field"
    assert props["mapField"]["title"] == "map_field"
    assert "title" not in props["messageField"]
    assert "title" not in props["explicitEnumField"]


def test_json_schema_recursive() -> None:
    defs = TypeAdapter(FileDescriptorSet).json_schema()["$defs"]
    nested_type = defs["DescriptorProto"]["properties"]["nestedType"]
    assert nested_type["items"]["$ref"] == "#/$defs/DescriptorProto"


def test_json_schema_forbids_additional_properties() -> None:
    schema = TypeAdapter(MixedFields).json_schema()
    assert schema["additionalProperties"] is False
    assert schema["$defs"]["Bar"]["additionalProperties"] is False
    # A map's additionalProperties carries its value schema, not a boolean.
    assert schema["properties"]["mapField"]["additionalProperties"]["type"] == "integer"


def test_json_schema_string_encoded_scalars() -> None:
    props = TypeAdapter(Scalars).json_schema()["properties"]
    assert props["int64Field"]["type"] == "string"
    assert props["int64Field"]["pattern"] == "^-?[0-9]+$"
    assert props["int64Field"]["examples"] == ["0"]
    assert props["uint64Field"]["pattern"] == "^[0-9]+$"
    assert props["bytesField"]["contentEncoding"] == "base64"
    assert props["int32Field"]["type"] == "integer"
    assert "pattern" not in props["int32Field"]
