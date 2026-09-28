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
"""Protobuf Well-Known types."""

from __future__ import annotations

from .google.protobuf.any_pb import Any
from .google.protobuf.descriptor_pb import (
    DescriptorProto,
    Edition,
    EnumDescriptorProto,
    EnumOptions,
    EnumValueDescriptorProto,
    EnumValueOptions,
    ExtensionRangeOptions,
    FeatureSet,
    FeatureSetDefaults,
    FieldDescriptorProto,
    FieldOptions,
    FileDescriptorProto,
    FileDescriptorSet,
    FileOptions,
    GeneratedCodeInfo,
    MessageOptions,
    MethodDescriptorProto,
    MethodOptions,
    OneofDescriptorProto,
    OneofOptions,
    ServiceDescriptorProto,
    ServiceOptions,
    SourceCodeInfo,
    SymbolVisibility,
    UninterpretedOption,
)
from .google.protobuf.duration_pb import Duration
from .google.protobuf.empty_pb import Empty
from .google.protobuf.field_mask_pb import FieldMask
from .google.protobuf.struct_pb import ListValue, NullValue, Struct, Value
from .google.protobuf.timestamp_pb import Timestamp
from .google.protobuf.wrappers_pb import (
    BoolValue,
    BytesValue,
    DoubleValue,
    FloatValue,
    Int32Value,
    Int64Value,
    StringValue,
    UInt32Value,
    UInt64Value,
)

__all__ = [
    "Any",
    "BoolValue",
    "BytesValue",
    "DescriptorProto",
    "DoubleValue",
    "Duration",
    "Edition",
    "Empty",
    "EnumDescriptorProto",
    "EnumOptions",
    "EnumValueDescriptorProto",
    "EnumValueOptions",
    "ExtensionRangeOptions",
    "FeatureSet",
    "FeatureSetDefaults",
    "FieldDescriptorProto",
    "FieldMask",
    "FieldOptions",
    "FileDescriptorProto",
    "FileDescriptorSet",
    "FileOptions",
    "FloatValue",
    "GeneratedCodeInfo",
    "Int32Value",
    "Int64Value",
    "ListValue",
    "MessageOptions",
    "MethodDescriptorProto",
    "MethodOptions",
    "NullValue",
    "OneofDescriptorProto",
    "OneofOptions",
    "ServiceDescriptorProto",
    "ServiceOptions",
    "SourceCodeInfo",
    "StringValue",
    "Struct",
    "SymbolVisibility",
    "Timestamp",
    "UInt32Value",
    "UInt64Value",
    "UninterpretedOption",
    "Value",
]
