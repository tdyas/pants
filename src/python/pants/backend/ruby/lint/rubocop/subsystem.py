# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from pants.option.option_types import SkipOption, StrOption
from pants.option.subsystem import Subsystem

DEFAULT_VERSION = "1.27.0"


class RuboCopSubsystem(Subsystem):
    options_scope = "rubocop"
    name = "rubocop"
    help = "RuboCop, The Ruby Linter/Formatter that Serves and Protects (https://rubocop.org/)"

    version = StrOption("--version", default=DEFAULT_VERSION, help="Version of RuboCop to install")

    skip = SkipOption("fmt", "lint")
