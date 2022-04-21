# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).
from __future__ import annotations

import os

from pants.engine.environment import Environment
from pants.option.option_types import StrListOption
from pants.option.subsystem import Subsystem
from pants.util.ordered_set import OrderedSet
from pants.util.strutil import softwrap


class RubyGemSubsystem(Subsystem):
    options_scope = "rubygems"
    name = "gem"
    help = "RubyGems (https://rubygems.org/)"

    _search_paths = StrListOption(
        "--search-paths",
        default=["<PYENV>", "<PATH>"],
        help=softwrap(
            """
            A list of paths to search for the RubyGem `gem` binary.

            You can specify absolute paths to a `gem` binary
            and/or to directories containing the `gem` binary.

            The following special strings are supported:

              * `<PATH>`, the contents of the PATH env var
            """
        ),
        advanced=True,
        metavar="<binary-paths>",
    )

    def search_paths(self, env: Environment) -> tuple[str, ...]:
        def iter_path_entries():
            for entry in self._search_paths:
                if entry == "<PATH>":
                    path = env.get("PATH")
                    if path:
                        yield from path.split(os.pathsep)
                else:
                    yield entry

        return tuple(OrderedSet(iter_path_entries()))
