# Copyright 2024 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

from pants.option.option_types import BoolOption, IntOption, StrOption
from pants.option.subsystem import Subsystem
from pants.util.strutil import help_text


class PythonAwsCodeartifact(Subsystem):
    options_scope = "python-aws-codeartifact"
    help = help_text(
        """
        AWS CodeArtifact configuration

        These options are used to configure renewing an AWS CodeArtifact key to access a PyPi-compatible CodeArtifact
        repository.
        """
    )

    enabled = BoolOption(default=False, help="enable codeartifact key renewals")
    domain = StrOption(default="", help="CodeArtifact domain containing the repositories")
