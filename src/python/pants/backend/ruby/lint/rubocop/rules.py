# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).
import logging
from dataclasses import dataclass

from pants.backend.ruby.lint.rubocop import skip_field
from pants.backend.ruby.lint.rubocop.skip_field import SkipRuboCopField
from pants.backend.ruby.lint.rubocop.subsystem import RuboCopSubsystem
from pants.backend.ruby.target_types import RubySourceField
from pants.backend.ruby.util_rules.ruby_binaries import RubyGemBinary, RubyBinary
from pants.core.goals.fmt import FmtRequest, FmtResult
from pants.engine.fs import CreateDigest, Digest, Directory, DigestContents
from pants.engine.internals.native_engine import RemovePrefix, Snapshot
from pants.engine.internals.selectors import Get
from pants.engine.process import Process, ProcessResult
from pants.engine.rules import collect_rules, rule
from pants.engine.target import FieldSet, Target
from pants.engine.unions import UnionRule
from pants.util.logging import LogLevel
from pants.util.strutil import pluralize

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class RuboCopFieldSet(FieldSet):
    required_fields = (RubySourceField,)

    source: RubySourceField

    @classmethod
    def opt_out(cls, tgt: Target) -> bool:
        return tgt.get(SkipRuboCopField).value


class RuboCopRequest(FmtRequest):
    field_set_type = RuboCopFieldSet
    name = RuboCopSubsystem.options_scope


@dataclass(frozen=True)
class RuboCopSetup:
    digest: Digest
    path: str


@rule(desc="Download RubuCop", level=LogLevel.DEBUG)
async def rubocop_setup(
    rubocop_subsystem: RuboCopSubsystem, rubygem: RubyGemBinary
) -> RuboCopSetup:
    output_dir = "__output__"
    empty_output_dir = await Get(Digest, CreateDigest([Directory(output_dir)]))

    install_rubocop_result = await Get(
        ProcessResult,
        Process(
            argv=[
                rubygem.path,
                "install",
                "--no-document",
                "--env-shebang",
                f"--install-dir={output_dir}",
                f"--version={rubocop_subsystem.version}",
                f"rubocop",
            ],
            input_digest=empty_output_dir,
            output_directories=[output_dir],
            description="Download RuboCop",
            level=LogLevel.DEBUG,
        ),
    )

    rubocop_digest = await Get(
        Digest, RemovePrefix(install_rubocop_result.output_digest, output_dir)
    )
    # ss = await Get(Snapshot, Digest, rubocop_digest)
    # print(f"rubocop.files: {ss.files}")

    return RuboCopSetup(rubocop_digest, "bin/rubocop")


@rule(desc="Format with RuboCop", level=LogLevel.DEBUG)
async def rubocop_fmt(
    request: RuboCopRequest, rubocop_subsystem: RuboCopSubsystem, rubocop_setup: RuboCopSetup, ruby: RubyBinary,
) -> FmtResult:
    if rubocop_subsystem.skip:
        return FmtResult.skip(formatter_name=request.name)

    immutable_input_digests = {
        "__rubocop": rubocop_setup.digest,
    }

    entries = await Get(DigestContents, Digest, rubocop_setup.digest)
    for entry in entries:
        if entry.path == "bin/rubocop":
            print(f"bin/rubocop:\n{entry.content.decode()}")

    print(f"ss: {request.snapshot.files}")

    result = await Get(
        ProcessResult,
        Process(
            argv=[
                ruby.path,
                f"__rubocop/{rubocop_setup.path}",
                "-x",  # only fix formatting-related offenses
                *request.snapshot.files,
            ],
            env={
                "GEM_HOME": "__rubocop",
                "GEM_PATH": "__rubocop",
            },
            input_digest=request.snapshot.digest,
            immutable_input_digests=immutable_input_digests,
            output_files=request.snapshot.files,
            description=f"Run RuboCop on {pluralize(len(request.field_sets), 'file')}.",
            level=LogLevel.DEBUG,
        ),
    )

    output_snapshot = await Get(Snapshot, Digest, result.output_digest)
    return FmtResult.create(request, result, output_snapshot, strip_chroot_path=True)


def rules():
    return [
        *collect_rules(),
        *skip_field.rules(),
        UnionRule(FmtRequest, RuboCopRequest),
    ]
