# Copyright 2019 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

import itertools
import logging
from collections import defaultdict
from dataclasses import dataclass
from typing import Any, Iterable, Iterator, NamedTuple, Sequence, Tuple, Type

from pants.base.specs import Specs
from pants.core.goals.lint import (
    LintFilesRequest,
    LintRequest,
    LintResult,
    LintTargetsRequest,
    _get_partitions_by_request_type,
)
from pants.core.goals.multi_tool_goal_helper import BatchSizeOption, OnlyOption
from pants.core.util_rules.partitions import PartitionerType, PartitionMetadataT
from pants.core.util_rules.partitions import Partitions as UntypedPartitions
from pants.engine.collection import Collection
from pants.engine.console import Console
from pants.engine.engine_aware import EngineAwareReturnType
from pants.engine.environment import EnvironmentName
from pants.engine.fs import Digest, MergeDigests, PathGlobs, Snapshot, SnapshotDiff, Workspace
from pants.engine.goal import Goal, GoalSubsystem
from pants.engine.process import FallibleProcessResult, ProcessResult
from pants.engine.rules import Get, MultiGet, collect_rules, goal_rule, rule, rule_helper
from pants.engine.unions import UnionMembership, UnionRule, distinct_union_type_per_subclass, union
from pants.util.collections import partition_sequentially
from pants.util.logging import LogLevel
from pants.util.meta import frozen_after_init
from pants.util.strutil import strip_v2_chroot_path

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class FmtResult(EngineAwareReturnType):
    input: Snapshot
    output: Snapshot
    stdout: str
    stderr: str
    formatter_name: str

    @staticmethod
    @rule_helper(_public=True)
    async def create(
        request: FmtRequest.Batch,
        process_result: ProcessResult | FallibleProcessResult,
        *,
        strip_chroot_path: bool = False,
    ) -> FmtResult:
        def prep_output(s: bytes) -> str:
            return strip_v2_chroot_path(s) if strip_chroot_path else s.decode()

        return FmtResult(
            input=request.snapshot,
            output=await Get(Snapshot, Digest, process_result.output_digest),
            stdout=prep_output(process_result.stdout),
            stderr=prep_output(process_result.stderr),
            formatter_name=request.tool_name,
        )

    def __post_init__(self):
        # NB: We debug log stdout/stderr because `message` doesn't log it.
        log = f"Output from {self.formatter_name}"
        if self.stdout:
            log += f"\n{self.stdout}"
        if self.stderr:
            log += f"\n{self.stderr}"
        logger.debug(log)

    @property
    def did_change(self) -> bool:
        return self.output != self.input

    def level(self) -> LogLevel | None:
        return LogLevel.WARN if self.did_change else LogLevel.INFO

    def message(self) -> str | None:
        message = "made changes." if self.did_change else "made no changes."

        # NB: Instead of printing out `stdout` and `stderr`, we just print a list of files which
        # were changed/added/removed. We do this for two reasons:
        #   1. This is run as part of both `fmt` and `lint`, and we want consistent output between both
        #   2. Different formatters have different stdout/stderr. This way is consistent across all
        #       formatters.
        # We also allow added/removed files because the `fmt` goal is more like a `fix` goal.
        # (See https://github.com/pantsbuild/pants/issues/13504)
        if self.did_change:
            snapshot_diff = SnapshotDiff.from_snapshots(self.input, self.output)
            output = "".join(
                f"\n  {file}"
                for file in itertools.chain(
                    snapshot_diff.changed_files,
                    snapshot_diff.their_unique_files,  # added files
                    snapshot_diff.our_unique_files,  # removed files
                    # NB: there is no rename detection, so a renames will list
                    # both the old filename (removed) and the new filename (added).
                )
            )
        else:
            output = ""

        return f"{self.formatter_name} {message}{output}"

    def cacheable(self) -> bool:
        """Is marked uncacheable to ensure that it always renders."""
        return False


Partitions = UntypedPartitions[str, PartitionMetadataT]


@union
class FmtRequest(LintRequest):
    is_formatter = True

    @distinct_union_type_per_subclass(in_scope_types=[EnvironmentName])
    @frozen_after_init
    @dataclass(unsafe_hash=True)
    class Batch(LintRequest.Batch):
        snapshot: Snapshot

        @property
        def files(self) -> tuple[str, ...]:
            return self.elements

    @classmethod
    def _get_rules(cls) -> Iterable[UnionRule]:
        yield from super()._get_rules()
        yield UnionRule(FmtRequest, cls)
        yield UnionRule(FmtRequest.Batch, cls.Batch)


class FmtTargetsRequest(FmtRequest, LintTargetsRequest):
    @classmethod
    def _get_rules(cls) -> Iterable:
        yield from cls.partitioner_type.default_rules(cls, by_file=True)
        yield from (
            rule
            for rule in super()._get_rules()
            # NB: We don't want to yield `lint.py`'s default partitioner
            if isinstance(rule, UnionRule)
        )
        yield UnionRule(FmtTargetsRequest.PartitionRequest, cls.PartitionRequest)


class FmtFilesRequest(FmtRequest, LintFilesRequest):
    @classmethod
    def _get_rules(cls) -> Iterable:
        if cls.partitioner_type is not PartitionerType.CUSTOM:
            raise ValueError(
                "Pants does not provide default partitioners for `FmtFilesRequest`."
                + " You will need to provide your own partitioner rule."
            )

        yield from super()._get_rules()
        yield UnionRule(FmtFilesRequest.PartitionRequest, cls.PartitionRequest)


class _FmtBatchElement(NamedTuple):
    request_type: type[FmtRequest.Batch]
    tool_name: str
    files: tuple[str, ...]
    key: Any


class _FmtBatchRequest(Collection[_FmtBatchElement]):
    """Request to serially format all the elements in the given batch."""


@dataclass(frozen=True)
class _FmtBatchResult:
    results: tuple[FmtResult, ...]

    @property
    def did_change(self) -> bool:
        return any(result.did_change for result in self.results)


class FmtSubsystem(GoalSubsystem):
    name = "fmt"
    help = "Autoformat source code."

    @classmethod
    def activated(cls, union_membership: UnionMembership) -> bool:
        return FmtRequest in union_membership

    only = OnlyOption("formatter", "isort", "shfmt")
    batch_size = BatchSizeOption(uppercase="Formatter", lowercase="formatter")


class Fmt(Goal):
    subsystem_cls = FmtSubsystem
    environment_behavior = Goal.EnvironmentBehavior.LOCAL_ONLY  # TODO(#17129) — Migrate this.


@rule_helper
async def _write_files(workspace: Workspace, batched_results: Iterable[_FmtBatchResult]):
    if any(batched_result.did_change for batched_result in batched_results):
        # NB: this will fail if there are any conflicting changes, which we want to happen rather
        # than silently having one result override the other. In practice, this should never
        # happen due to us grouping each file's formatters into a single digest.
        merged_formatted_digest = await Get(
            Digest,
            MergeDigests(
                batched_result.results[-1].output.digest for batched_result in batched_results
            ),
        )
        workspace.write_digest(merged_formatted_digest)


def _print_results(
    console: Console,
    results: Iterable[FmtResult],
):
    if results:
        console.print_stderr("")

    # We group all results for the same formatter so that we can give one final status in the
    # summary. This is only relevant if there were multiple results because of
    # `--per-file-caching`.
    formatter_to_results = defaultdict(set)
    for result in results:
        formatter_to_results[result.formatter_name].add(result)

    for formatter, results in sorted(formatter_to_results.items()):
        if any(result.did_change for result in results):
            sigil = console.sigil_succeeded_with_edits()
            status = "made changes"
        else:
            sigil = console.sigil_succeeded()
            status = "made no changes"
        console.print_stderr(f"{sigil} {formatter} {status}.")


@goal_rule
async def fmt(
    console: Console,
    specs: Specs,
    fmt_subsystem: FmtSubsystem,
    workspace: Workspace,
    union_membership: UnionMembership,
) -> Fmt:
    fmt_request_types = list(union_membership.get(FmtRequest))
    target_partitioners = list(union_membership.get(FmtTargetsRequest.PartitionRequest))
    file_partitioners = list(union_membership.get(FmtFilesRequest.PartitionRequest))

    partitions_by_request_type = await _get_partitions_by_request_type(
        fmt_request_types,
        target_partitioners,
        file_partitioners,
        fmt_subsystem,
        specs,
        lambda request_type: Get(Partitions, FmtTargetsRequest.PartitionRequest, request_type),
        lambda request_type: Get(Partitions, FmtFilesRequest.PartitionRequest, request_type),
    )

    if not partitions_by_request_type:
        return Fmt(exit_code=0)

    def batch_by_size(files: Iterable[str]) -> Iterator[tuple[str, ...]]:
        batches = partition_sequentially(
            files,
            key=lambda x: str(x),
            size_target=fmt_subsystem.batch_size,
            size_max=4 * fmt_subsystem.batch_size,
        )
        for batch in batches:
            yield tuple(batch)

    def _make_disjoint_batch_requests() -> Iterable[_FmtBatchRequest]:
        partition_infos: Sequence[Tuple[Type[FmtRequest], Any]]
        files: Sequence[str]

        partition_infos_by_files = defaultdict(list)
        for request_type, partitions_list in partitions_by_request_type.items():
            for partitions in partitions_list:
                for partition in partitions:
                    for file in partition.elements:
                        partition_infos_by_files[file].append((request_type, partition.metadata))

        files_by_partition_info = defaultdict(list)
        for file, partition_infos in partition_infos_by_files.items():
            files_by_partition_info[tuple(partition_infos)].append(file)

        for partition_infos, files in files_by_partition_info.items():
            for batch in batch_by_size(files):
                yield _FmtBatchRequest(
                    _FmtBatchElement(
                        request_type.Batch,
                        request_type.tool_name,
                        batch,
                        partition_metadata,
                    )
                    for request_type, partition_metadata in partition_infos
                )

    all_results = await MultiGet(
        Get(_FmtBatchResult, _FmtBatchRequest, request)
        for request in _make_disjoint_batch_requests()
    )

    individual_results = list(
        itertools.chain.from_iterable(result.results for result in all_results)
    )

    await _write_files(workspace, all_results)
    _print_results(console, individual_results)

    # Since the rules to produce FmtResult should use ExecuteRequest, rather than
    # FallibleProcess, we assume that there were no failures.
    return Fmt(exit_code=0)


@rule
async def fmt_batch(
    request: _FmtBatchRequest,
) -> _FmtBatchResult:
    current_snapshot = await Get(Snapshot, PathGlobs(request[0].files))

    results = []
    for request_type, tool_name, files, key in request:
        batch = request_type(tool_name, files, key, current_snapshot)
        result = await Get(FmtResult, FmtRequest.Batch, batch)
        results.append(result)

        assert set(result.output.files) == set(
            batch.files
        ), f"Expected {result.output.files} to match {batch.files}"
        current_snapshot = result.output
    return _FmtBatchResult(tuple(results))


@rule(level=LogLevel.DEBUG)
async def convert_fmt_result_to_lint_result(fmt_result: FmtResult) -> LintResult:
    return LintResult(
        1 if fmt_result.did_change else 0,
        fmt_result.stdout,
        fmt_result.stderr,
        linter_name=fmt_result.formatter_name,
        _render_message=False,  # Don't re-render the message
    )


def rules():
    return collect_rules()
