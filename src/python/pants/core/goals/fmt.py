# Copyright 2019 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

import itertools
from abc import ABCMeta
from dataclasses import dataclass
from typing import ClassVar, Iterable, List, Optional, Tuple, Type

from pants.core.goals.lint import LinterFieldSet
from pants.core.util_rules.filter_empty_sources import TargetsWithSources, TargetsWithSourcesRequest
from pants.engine.collection import Collection
from pants.engine.console import Console
from pants.engine.fs import (
    EMPTY_DIGEST,
    Digest,
    DirectoryToMaterialize,
    MergeDigests,
    Snapshot,
    Workspace,
)
from pants.engine.goal import Goal, GoalSubsystem
from pants.engine.process import ProcessResult
from pants.engine.rules import goal_rule
from pants.engine.selectors import Get, MultiGet
from pants.engine.target import Field, Target, TargetsWithOrigins
from pants.engine.unions import UnionMembership, union


@dataclass(frozen=True)
class FmtResult:
    input: Digest
    output: Digest
    stdout: str
    stderr: str

    @staticmethod
    def noop() -> "FmtResult":
        return FmtResult(input=EMPTY_DIGEST, output=EMPTY_DIGEST, stdout="", stderr="")

    @staticmethod
    def from_process_result(
        process_result: ProcessResult, *, original_digest: Digest
    ) -> "FmtResult":
        return FmtResult(
            input=original_digest,
            output=process_result.output_digest,
            stdout=process_result.stdout.decode(),
            stderr=process_result.stderr.decode(),
        )

    @property
    def did_change(self) -> bool:
        return self.output != self.input


class FmtFieldSet(LinterFieldSet, metaclass=ABCMeta):
    """The fields necessary for a particular auto-formatter to work with a target."""


class FmtFieldSets(Collection[FmtFieldSet]):
    """A collection of `FieldSet`s for a particular formatter, e.g. a collection of
    `IsortFieldSet`s."""

    field_set_type: ClassVar[Type[FmtFieldSet]]

    def __init__(
        self,
        field_sets: Iterable[FmtFieldSet],
        *,
        prior_formatter_result: Optional[Snapshot] = None
    ) -> None:
        super().__init__(field_sets)
        self.prior_formatter_result = prior_formatter_result


@union
@dataclass(frozen=True)
class LanguageFmtTargets:
    """All the targets that belong together as one language, e.g. all Python targets.

    This allows us to group distinct formatters by language as a performance optimization. Within a
    language, each formatter must run sequentially to not overwrite the previous formatter; but
    across languages, it is safe to run in parallel.
    """

    required_fields: ClassVar[Tuple[Type[Field], ...]]

    targets_with_origins: TargetsWithOrigins

    @classmethod
    def belongs_to_language(cls, tgt: Target) -> bool:
        return tgt.has_fields(cls.required_fields)


@dataclass(frozen=True)
class LanguageFmtResults:
    """This collection allows us to safely aggregate multiple `FmtResult`s for a language.

    The `output` digest is used to ensure that none of the formatters overwrite each other. The
    language implementation should run each formatter one at a time and pipe the resulting digest of
    one formatter into the next. The `input` and `output` digests must contain all files for the
    target(s), including any which were not re-formatted.
    """

    results: Tuple[FmtResult, ...]
    input: Digest
    output: Digest

    @property
    def did_change(self) -> bool:
        return self.input != self.output


class FmtOptions(GoalSubsystem):
    """Autoformat source code."""

    name = "fmt"

    required_union_implementations = (LanguageFmtTargets,)

    @classmethod
    def register_options(cls, register) -> None:
        super().register_options(register)
        register(
            "--only",
            type=str,
            default=None,
            fingerprint=True,
            advanced=True,
            help=(
                "Only run the specified formatter. Currently the only accepted values are "
                "`scalafix` or not setting any value."
            ),
        )
        register(
            "--per-target-caching",
            advanced=True,
            type=bool,
            default=False,
            help=(
                "Rather than running all targets in a single batch, run each target as a "
                "separate process. Why do this? You'll get many more cache hits. Why not do this? "
                "Formatters both have substantial startup overhead and are cheap to add one "
                "additional file to the run. On a cold cache, it is much faster to use "
                "`--no-per-target-caching`. We only recommend using `--per-target-caching` if you "
                "are using a remote cache or if you have benchmarked that this option will be "
                "faster than `--no-per-target-caching` for your use case."
            ),
        )


class Fmt(Goal):
    subsystem_cls = FmtOptions


@goal_rule
async def fmt(
    console: Console,
    targets_with_origins: TargetsWithOrigins,
    options: FmtOptions,
    workspace: Workspace,
    union_membership: UnionMembership,
) -> Fmt:
    language_target_collection_types: Iterable[Type[LanguageFmtTargets]] = (
        union_membership.union_rules[LanguageFmtTargets]
    )

    language_target_collections: Iterable[LanguageFmtTargets] = tuple(
        language_target_collection_type(
            TargetsWithOrigins(
                target_with_origin
                for target_with_origin in targets_with_origins
                if language_target_collection_type.belongs_to_language(target_with_origin.target)
            )
        )
        for language_target_collection_type in language_target_collection_types
    )
    targets_with_sources: Iterable[TargetsWithSources] = await MultiGet(
        Get[TargetsWithSources](
            TargetsWithSourcesRequest(
                target_with_origin.target
                for target_with_origin in language_target_collection.targets_with_origins
            )
        )
        for language_target_collection in language_target_collections
    )
    # NB: We must convert back the generic TargetsWithSources objects back into their
    # corresponding LanguageFmtTargets, e.g. back to PythonFmtTargets, in order for the union
    # rule to work.
    valid_language_target_collections: Iterable[LanguageFmtTargets] = tuple(
        language_target_collection_cls(
            TargetsWithOrigins(
                target_with_origin
                for target_with_origin in language_target_collection.targets_with_origins
                if target_with_origin.target in language_targets_with_sources
            )
        )
        for language_target_collection_cls, language_target_collection, language_targets_with_sources in zip(
            language_target_collection_types, language_target_collections, targets_with_sources
        )
        if language_targets_with_sources
    )

    if options.values.per_target_caching:
        per_language_results = await MultiGet(
            Get[LanguageFmtResults](
                LanguageFmtTargets,
                language_target_collection.__class__(TargetsWithOrigins([target_with_origin])),
            )
            for language_target_collection in valid_language_target_collections
            for target_with_origin in language_target_collection.targets_with_origins
        )
    else:
        per_language_results = await MultiGet(
            Get[LanguageFmtResults](LanguageFmtTargets, language_target_collection)
            for language_target_collection in valid_language_target_collections
        )

    individual_results: List[FmtResult] = list(
        itertools.chain.from_iterable(
            language_result.results for language_result in per_language_results
        )
    )

    if not individual_results:
        return Fmt(exit_code=0)

    changed_digests = tuple(
        language_result.output
        for language_result in per_language_results
        if language_result.did_change
    )
    if changed_digests:
        # NB: this will fail if there are any conflicting changes, which we want to happen rather
        # than silently having one result override the other. In practicality, this should never
        # happen due to us grouping each language's formatters into a single digest.
        merged_formatted_digest = await Get[Digest](MergeDigests(changed_digests))
        workspace.materialize_directory(DirectoryToMaterialize(merged_formatted_digest))

    for result in individual_results:
        if result.stdout:
            console.print_stdout(result.stdout)
        if result.stderr:
            console.print_stderr(result.stderr)

    # Since the rules to produce FmtResult should use ExecuteRequest, rather than
    # FallibleProcess, we assume that there were no failures.
    return Fmt(exit_code=0)


def rules():
    return [fmt]
