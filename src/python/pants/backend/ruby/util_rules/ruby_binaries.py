# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).
from pants.backend.ruby.subsystems.ruby import RubySubsystem
from pants.backend.ruby.subsystems.rubygems import RubyGemSubsystem
from pants.core.util_rules.system_binaries import (
    BinaryNotFoundError,
    BinaryPath,
    BinaryPathRequest,
    BinaryPaths,
    BinaryPathTest,
)
from pants.engine.environment import Environment, EnvironmentRequest
from pants.engine.internals.selectors import Get, MultiGet
from pants.engine.rules import collect_rules, rule
from pants.util.logging import LogLevel


class RubyBinary(BinaryPath):
    """A Ruby interpreter for use by `@rule` code."""


class RubyGemBinary(BinaryPath):
    """The `gem` RubyGem insaller binary."""


@rule(desc="Finding a `ruby` binary", level=LogLevel.TRACE)
async def find_ruby(ruby_subsystem: RubySubsystem) -> RubyBinary:
    env = await Get(Environment, EnvironmentRequest(["PATH"]))
    interpreter_search_paths = ruby_subsystem.search_paths(env)
    all_ruby_binary_paths = await MultiGet(
        Get(
            BinaryPaths,
            BinaryPathRequest(
                search_path=interpreter_search_paths,
                binary_name=binary_name,
                check_file_entries=True,
                test=BinaryPathTest(args=["--version"]),
            ),
        )
        for binary_name in ruby_subsystem.names
    )

    for binary_paths in all_ruby_binary_paths:
        path = binary_paths.first_path
        if path:
            return RubyBinary(
                path=path.path,
                fingerprint=path.fingerprint,
            )

    raise BinaryNotFoundError(
        "Was not able to locate a Ruby interpreter.\n"
        "Please ensure that Ruby is available in one of the locations identified by "
        "`[ruby].search_paths`, which currently expands to:\n"
        f"  {interpreter_search_paths}"
    )


@rule(desc="Finding a `gem` binary", level=LogLevel.TRACE)
async def find_rubygem(rubygem_subsystem: RubyGemSubsystem) -> RubyGemBinary:
    env = await Get(Environment, EnvironmentRequest(["PATH"]))
    rubygem_binary_paths = await Get(
        BinaryPaths,
        BinaryPathRequest(
            search_path=rubygem_subsystem.search_paths(env),
            binary_name="gem",
            check_file_entries=True,
            test=BinaryPathTest(args=["-v"]),
        ),
    )

    path = rubygem_binary_paths.first_path
    if path:
        return RubyGemBinary(
            path=path.path,
            fingerprint=path.fingerprint,
        )

    raise BinaryNotFoundError(
        "Was not able to locate `gem`.\n"
        "Please ensure that Ruby is available in one of the locations identified by "
        "`[rubygems].search_paths`, which currently expands to:\n"
        f"  {rubygem_subsystem.search_paths(env)}"
    )


def rules():
    return collect_rules()
