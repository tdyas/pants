# Copyright 2020 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

"""Generate Python sources from Protocol Buffers (Protobufs).

See https://developers.google.com/protocol-buffers/.
"""

from pants.backend.codegen.protobuf.python import additional_fields
from pants.backend.codegen.protobuf.python.rules import rules as python_rules
from pants.backend.codegen.protobuf.target_types import ProtobufLibrary


def rules():
    return [*additional_fields.rules(), *python_rules()]


def target_types():
    return [ProtobufLibrary]
