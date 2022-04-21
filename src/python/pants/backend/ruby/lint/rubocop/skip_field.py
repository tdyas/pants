# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from pants.backend.ruby.target_types import RubySourcesGeneratorTarget, RubySourceTarget
from pants.engine.target import BoolField


class SkipRuboCopField(BoolField):
    alias = "skip_rubocop"
    default = False
    help = "If true, don't run RuboCop on this target's code."


def rules():
    return [
        RubySourceTarget.register_plugin_field(SkipRuboCopField),
        RubySourcesGeneratorTarget.register_plugin_field(SkipRuboCopField),
    ]
