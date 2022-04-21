# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).
from __future__ import annotations

import os

from pants.engine.environment import Environment
from pants.option.option_types import StrListOption
from pants.option.subsystem import Subsystem
from pants.util.ordered_set import OrderedSet
from pants.util.strutil import softwrap


class RubySubsystem(Subsystem):
    options_scope = "ruby"
    name = "ruby"
    help = "Ruby programming language (https://www.ruby-lang.org/)"

    _search_path = StrListOption(
        "--search-paths",
        default=["<PYENV>", "<PATH>"],
        help=softwrap(
            """
            A list of paths to search for Ruby interpreters.

            Which interpreters are actually used from these paths is context-specific:
            the Ruby backend selects particular interpreters using the
            `[ruby].interpreter_constraints` option.

            You can specify absolute paths to interpreter binaries
            and/or to directories containing interpreter binaries. The order of entries does
            not matter.

            The following special strings are supported:

              * `<PATH>`, the contents of the PATH env var
            """
        ),
        advanced=True,
        metavar="<binary-paths>",
    )
    names = StrListOption(
        "--names",
        default=["ruby"],
        help=softwrap(
            """
            The names of Python binaries to search for. See the `--search-path` option to
            influence where interpreters are searched for.

            This does not impact which Python interpreter is used to run your code, only what
            is used to run internal tools.
            """
        ),
        advanced=True,
        metavar="<python-binary-names>",
    )

    def search_paths(self, env: Environment) -> tuple[str, ...]:
        def iter_path_entries():
            for entry in self._search_path:
                if entry == "<PATH>":
                    path = env.get("PATH")
                    if path:
                        yield from path.split(os.pathsep)
                else:
                    yield entry

        return tuple(OrderedSet(iter_path_entries()))
