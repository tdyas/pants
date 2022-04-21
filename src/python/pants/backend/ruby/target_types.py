# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

from dataclasses import dataclass

from pants.engine.rules import collect_rules
from pants.engine.target import (
    COMMON_TARGET_FIELDS,
    Dependencies,
    FieldSet,
    MultipleSourcesField,
    SingleSourceField,
    Target,
    TargetFilesGenerator,
)


class RubySourceField(SingleSourceField):
    expected_file_extensions = (".rb",)


class RubyGeneratorSourcesField(MultipleSourcesField):
    expected_file_extensions = (".rb",)


@dataclass(frozen=True)
class RubyFieldSet(FieldSet):
    required_fields = (RubySourceField,)

    sources: RubySourceField


@dataclass(frozen=True)
class RubyGeneratorFieldSet(FieldSet):
    required_fields = (RubyGeneratorSourcesField,)

    sources: RubyGeneratorSourcesField


class RubyDependenciesField(Dependencies):
    pass


# -----------------------------------------------------------------------------------------------
# `ruby_source` and `ruby_sources` targets
# -----------------------------------------------------------------------------------------------


class RubySourceTarget(Target):
    alias = "ruby_source"
    core_fields = (
        *COMMON_TARGET_FIELDS,
        RubyDependenciesField,
        RubySourceField,
    )
    help = "A single Ruby source file containing application or library code."


class RubySourcesGeneratorSourcesField(RubyGeneratorSourcesField):
    default = ("*.rb",)


class RubySourcesGeneratorTarget(TargetFilesGenerator):
    alias = "ruby_sources"
    core_fields = (
        *COMMON_TARGET_FIELDS,
        RubySourcesGeneratorSourcesField,
    )
    generated_target_cls = RubySourceTarget
    copied_fields = COMMON_TARGET_FIELDS
    moved_fields = (RubyDependenciesField,)
    help = "Generate a `ruby_source` target for each file in the `sources` field."


def rules():
    return collect_rules()
