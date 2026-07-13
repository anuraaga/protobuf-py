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

from pydantic import BaseModel, TypeAdapter

from protobuf.wkt import Duration, Timestamp

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


class Model(BaseModel):
    msg: Scalars


def test_model() -> None:
    s = Scalars(uint32_field=124, bool_field=True, float_field=3.0)
    m = Model(msg=s)
    assert m.msg == s
    assert m.model_dump() == {"msg": s}
    assert m.model_dump_json() == f'{{"msg":{s.to_json()}}}'
