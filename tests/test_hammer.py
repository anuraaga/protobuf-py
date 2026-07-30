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
"""Stress tests for thread safety."""

from __future__ import annotations

import contextlib
import threading
from typing import TYPE_CHECKING

import pytest

from protobuf._native_message import NativeMessageClass
from tests.gen.messages_pb import MixedFields
from tests.gen.scalars_pb import Scalars

if TYPE_CHECKING:
    from collections.abc import Callable


_NUM_THREADS = 16
_ITERATIONS = 100_000
# Marshaling is far heavier per iteration than a bare attribute write, so the
# serialize-focused tests use a smaller count to keep wall time reasonable.
# Mutators keep containers bounded (periodic clears) so serialization stays
# cheap and total time does not blow up quadratically as fields grow.
_MARSHAL_ITERATIONS = 20_000


def _hammer(workers: list[Callable[[int], None]], iterations: int) -> None:
    barrier = threading.Barrier(len(workers))

    def spin(worker: Callable[[int], None]) -> None:
        barrier.wait()
        for i in range(iterations):
            # We are primarily confirming no segfaults. Message itself is documented
            # as not safe for concurrent mutation with reads, but we still don't allow
            # to actually crash the interpreter.
            with contextlib.suppress(BaseException):
                worker(i)

    threads = [threading.Thread(target=spin, args=(w,)) for w in workers]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()


def test_hammer_single_message_read_write() -> None:
    if NativeMessageClass is None:
        # Test not meaningful for pure Python.
        pytest.skip("Native marshaler not available")

    msg = Scalars()

    def worker(i: int) -> None:
        # Fresh, uniquely refcounted string each write to exercise racy refcounting.
        msg.string_field = f"i{i}-{'x' * 32}"
        _ = msg.string_field

    _hammer([worker] * _NUM_THREADS, _ITERATIONS)


# Below tests are for concurrent mutations with marshaling. This is documented as not
# safe, but we still confirm they don't cause crashes or corrupt payloads.


def test_hammer_mutate_and_serialize_binary() -> None:
    msg = MixedFields()

    def mutator(i: int) -> None:
        op = i % 7
        if op == 0:
            msg.explicit_field = i
        elif op == 1:
            msg.repeated_field.append(f"v{i}")
        elif op == 2:
            msg.map_field[f"k{i & 15}"] = i
        elif op == 3:
            msg.message_field = MixedFields.Bar(value=f"b{i}")
        elif op == 4:
            msg.clear_field("repeated_field")
        elif op == 5:
            msg.clear_field("map_field")
        else:
            msg.clear_field("message_field")

    def serializer(_: int) -> None:
        msg.to_binary()

    _hammer(
        [mutator] * (_NUM_THREADS // 2) + [serializer] * (_NUM_THREADS // 2),
        _MARSHAL_ITERATIONS,
    )


def test_hammer_mutate_and_serialize_json() -> None:
    msg = MixedFields()

    def mutator(i: int) -> None:
        op = i % 6
        if op == 0:
            msg.explicit_field = i
        elif op == 1:
            msg.repeated_field.append(f"v{i}")
        elif op == 2:
            msg.map_field[f"k{i & 15}"] = i
        elif op == 3:
            msg.message_field = MixedFields.Bar(value=f"b{i}")
        elif op == 4:
            msg.clear_field("repeated_field")
        else:
            msg.clear_field("map_field")

    def serializer(_: int) -> None:
        msg.to_json()

    _hammer(
        [mutator] * (_NUM_THREADS // 2) + [serializer] * (_NUM_THREADS // 2),
        _MARSHAL_ITERATIONS,
    )


def test_hammer_containers() -> None:
    msg = MixedFields()

    def repeated_writer(i: int) -> None:
        msg.repeated_field.append(f"v{i}")
        if i & 63 == 0:
            msg.clear_field("repeated_field")

    def map_writer(i: int) -> None:
        msg.map_field[f"k{i & 31}"] = i
        if i & 63 == 0:
            msg.clear_field("map_field")

    def reader(_: int) -> None:
        list(msg.repeated_field)
        dict(msg.map_field)

    def serializer(_: int) -> None:
        msg.to_binary()

    quarter = _NUM_THREADS // 4
    _hammer(
        [repeated_writer] * quarter
        + [map_writer] * quarter
        + [reader] * quarter
        + [serializer] * quarter,
        _MARSHAL_ITERATIONS,
    )


def test_hammer_roundtrip() -> None:
    msg = MixedFields(explicit_field=1, repeated_field=["seed"])
    latest: list[bytes] = [msg.to_binary()]
    parse_failures: list[BaseException] = []

    def mutator(i: int) -> None:
        msg.explicit_field = i
        msg.repeated_field.append(f"v{i}")
        if i & 31 == 0:
            msg.clear_field("repeated_field")

    def serializer(_: int) -> None:
        # latest[0] will not be updated when to_binary raises an error, which can
        # happen when iterating mutated collections. For the times when it is updated,
        # parser() will check the bytes actually parse.
        latest[0] = msg.to_binary()

    def parser(_: int) -> None:
        data = latest[0]
        try:
            MixedFields.from_binary(data)
        except BaseException as exc:  # noqa: BLE001 - recorded and asserted below
            parse_failures.append(exc)

    third = _NUM_THREADS // 3
    _hammer(
        [mutator] * third
        + [serializer] * third
        + [parser] * (_NUM_THREADS - 2 * third),
        _MARSHAL_ITERATIONS,
    )

    assert not parse_failures, (
        f"{len(parse_failures)} of the produced payloads failed to parse; "
        f"serialization emitted malformed bytes, e.g. {parse_failures[0]!r}"
    )
