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

import sys
from functools import cache
from typing import TYPE_CHECKING, cast

from ._descriptors import ScalarType

if TYPE_CHECKING:
    from collections.abc import Sized

    from ._message import Message

# Approximate sizes of CPython heap allocations, measured with `sys.getsizeof`
# on 64-bit CPython 3.14. The budget guards against unbounded allocation from
# malicious payloads rather than providing exact accounting, so small
# inaccuracies across versions and builds are fine.

GC_HEAD_SIZE = 16
"""GC header allocated in front of every GC-tracked object."""
FLOAT_SIZE = 24
"""A float object."""
INT_SIZE = 36
"""A 64-bit int object. Smaller ints are slightly smaller."""
STR_OVERHEAD = 41
"""Header of a compact ASCII str; the length is charged on top."""
BYTES_OVERHEAD = 33
"""Header of a bytes object."""
EMPTY_LIST_SIZE = 56
"""An empty list."""
EMPTY_DICT_SIZE = 64
"""An empty dict."""
LIST_SLOT_SIZE = 8
"""One appended list element: an 8-byte pointer slot."""
DICT_ENTRY_SIZE = 40
"""One inserted dict entry: hash + key + value words plus growth slack."""
ONEOF_SIZE = 32
"""A Oneof wrapper object: object header plus two references."""


@cache
def _base_alloc_size(message_type: type[Message]) -> int:
    """Approximate heap size of a freshly-initialized message instance.

    The fixed instance size (all fields are slots) plus the empty containers
    created for repeated/map field defaults.
    """
    size = message_type.__basicsize__ + GC_HEAD_SIZE
    for _, default in message_type._desc._defaults:
        if isinstance(default, list):
            size += EMPTY_LIST_SIZE
        elif isinstance(default, dict):
            size += EMPTY_DICT_SIZE
    return size


class Budget:
    """Tracks approximate allocations while parsing a message.

    Raises an error once the configured limit is exceeded.
    """

    __slots__ = ("current", "max")

    def __init__(self, limit: int | None = None) -> None:
        self.current = 0
        self.max = sys.maxsize if limit is None else limit

    def charge(self, amount: int) -> None:
        self.current += amount
        if self.current > self.max:
            msg = f"allocation budget exceeded: needed {self.current} bytes, limit is {self.max}"
            raise ValueError(msg)

    def charge_scalar(self, scalar_type: ScalarType, value: object) -> None:
        """Charges for one parsed scalar value of the given type."""
        if scalar_type == ScalarType.STRING:
            self.charge(STR_OVERHEAD + len(cast("Sized", value)))
        elif scalar_type == ScalarType.BYTES:
            self.charge(BYTES_OVERHEAD + len(cast("Sized", value)))
        elif scalar_type == ScalarType.BOOL:
            # Bools are shared singletons; nothing is allocated.
            pass
        elif scalar_type in (ScalarType.DOUBLE, ScalarType.FLOAT):
            self.charge(FLOAT_SIZE)
        else:
            self.charge(INT_SIZE)

    def charge_message(self, message_type: type[Message]) -> None:
        """Charges the base size of a new instance of the message type."""
        self.charge(_base_alloc_size(message_type))
