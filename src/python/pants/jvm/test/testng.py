# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).
from __future__ import annotations

from dataclasses import dataclass

from pants.core.goals.generate_lockfiles import GenerateToolLockfileSentinel
from pants.core.goals.test import (
    TestDebugAdapterRequest,
    TestDebugRequest,
    TestFieldSet,
    TestResult,
    TestSubsystem,
)
from pants.core.target_types import FileSourceField
from pants.core.util_rules.source_files import SourceFiles, SourceFilesRequest
from pants.engine.addresses import Addresses
from pants.engine.fs import CreateDigest, DigestSubset, Directory, PathGlobs
from pants.engine.internals.native_engine import Digest, MergeDigests, RemovePrefix, Snapshot
from pants.engine.internals.selectors import Get, MultiGet
from pants.engine.process import (
    FallibleProcessResult,
    InteractiveProcess,
    InteractiveProcessRequest,
    Process,
    ProcessCacheScope,
)
from pants.engine.rules import collect_rules, rule
from pants.engine.target import SourcesField, TransitiveTargets, TransitiveTargetsRequest
from pants.engine.unions import UnionRule
from pants.jvm.classpath import Classpath
from pants.jvm.goals import lockfile
from pants.jvm.jdk_rules import JdkEnvironment, JdkRequest, JvmProcess
from pants.jvm.resolve.coursier_fetch import ToolClasspath, ToolClasspathRequest
from pants.jvm.resolve.jvm_tool import GenerateJvmLockfileFromTool, JvmToolBase
from pants.jvm.subsystems import JvmSubsystem
from pants.jvm.target_types import JvmDependenciesField, JvmJdkField, TestNGTestSourceField
from pants.option.option_types import ArgsListOption
from pants.util.docutil import git_url
from pants.util.logging import LogLevel


class TestNG(JvmToolBase):
    options_scope = "testng"
    name = "TestNG"
    help = "The TestNG test framework (https://testng.org/)"

    default_version = "7.6.0"
    default_artifacts = ("org.testng:testng:{version}",)
    default_lockfile_resource = ("pants.jvm.test", "testng.lock")
    default_lockfile_path = "src/python/pants/jvm/test/testng.lock"
    default_lockfile_url = git_url(default_lockfile_path)

    args = ArgsListOption(example="--groups regression", passthrough=True)


@dataclass(frozen=True)
class TestNGTestFieldSet(TestFieldSet):
    required_fields = (
        TestNGTestSourceField,
        JvmJdkField,
    )

    sources: TestNGTestSourceField
    jdk_version: JvmJdkField
    dependencies: JvmDependenciesField


class TestNGToolLockfileSentinel(GenerateToolLockfileSentinel):
    resolve_name = TestNG.options_scope


@dataclass(frozen=True)
class TestSetupRequest:
    field_set: TestNGTestFieldSet
    is_debug: bool


@dataclass(frozen=True)
class TestSetup:
    process: JvmProcess
    reports_dir_prefix: str


@rule(level=LogLevel.DEBUG)
async def setup_testng_for_target(
    request: TestSetupRequest,
    jvm: JvmSubsystem,
    testng: TestNG,
    test_subsystem: TestSubsystem,
) -> TestSetup:
    jdk, transitive_tgts, lockfile_request = await MultiGet(
        Get(JdkEnvironment, JdkRequest, JdkRequest.from_field(request.field_set.jdk_version)),
        Get(TransitiveTargets, TransitiveTargetsRequest([request.field_set.address])),
        Get(GenerateJvmLockfileFromTool, TestNGToolLockfileSentinel()),
    )

    classpath, testng_classpath, files = await MultiGet(
        Get(Classpath, Addresses([request.field_set.address])),
        Get(ToolClasspath, ToolClasspathRequest(lockfile=lockfile_request)),
        Get(
            SourceFiles,
            SourceFilesRequest(
                (dep.get(SourcesField) for dep in transitive_tgts.dependencies),
                for_sources_types=(FileSourceField,),
                enable_codegen=True,
            ),
        ),
    )

    reports_dir_prefix = "__reports_dir"
    # reports_dir = f"{reports_dir_prefix}/{request.field_set.address.path_safe_spec}"
    reports_dir = reports_dir_prefix

    output_dir = await Get(Digest, CreateDigest([Directory(reports_dir)]))
    input_digest = await Get(
        Digest, MergeDigests((*classpath.digests(), files.snapshot.digest, output_dir))
    )

    toolcp_relpath = "__toolcp"
    extra_immutable_input_digests = {
        toolcp_relpath: testng_classpath.digest,
    }

    # Classfiles produced by the root `testng_test` targets are the only ones which should run.
    user_classpath_arg = ":".join(classpath.root_args())

    # Cache test runs only if they are successful, or not at all if `--test-force`.
    cache_scope = (
        ProcessCacheScope.PER_SESSION if test_subsystem.force else ProcessCacheScope.SUCCESSFUL
    )

    extra_jvm_args: list[str] = []
    if request.is_debug:
        extra_jvm_args.extend(jvm.debug_args)

    process = JvmProcess(
        jdk=jdk,
        classpath_entries=[
            # *classpath.args(),
            *testng_classpath.classpath_entries(toolcp_relpath),
        ],
        argv=[
            *extra_jvm_args,
            f"-Dtestng.test.classpath={user_classpath_arg}",
            "org.testng.TestNG",
            *testng.args,
        ],
        input_digest=input_digest,
        extra_jvm_options=testng.jvm_options,
        extra_immutable_input_digests=extra_immutable_input_digests,
        output_directories=(reports_dir,),
        description=f"Run TestNG for {request.field_set.address}",
        level=LogLevel.DEBUG,
        cache_scope=cache_scope,
        use_nailgun=False,
    )
    return TestSetup(process=process, reports_dir_prefix=reports_dir_prefix)


@rule(desc="Run TestNG", level=LogLevel.DEBUG)
async def run_testng_test(
    test_subsystem: TestSubsystem,
    field_set: TestNGTestFieldSet,
) -> TestResult:
    test_setup = await Get(TestSetup, TestSetupRequest(field_set, is_debug=False))
    process_result = await Get(FallibleProcessResult, JvmProcess, test_setup.process)
    reports_dir_prefix = test_setup.reports_dir_prefix

    xml_result_subset = await Get(
        Digest, DigestSubset(process_result.output_digest, PathGlobs([f"{reports_dir_prefix}/**"]))
    )
    xml_results = await Get(Snapshot, RemovePrefix(xml_result_subset, reports_dir_prefix))

    return TestResult.from_fallible_process_result(
        process_result,
        address=field_set.address,
        output_setting=test_subsystem.output,
        xml_results=xml_results,
    )


@rule(level=LogLevel.DEBUG)
async def setup_testng_debug_request(field_set: TestNGTestFieldSet) -> TestDebugRequest:
    setup = await Get(TestSetup, TestSetupRequest(field_set, is_debug=True))
    process = await Get(Process, JvmProcess, setup.process)
    interactive_process = await Get(
        InteractiveProcess,
        InteractiveProcessRequest(process, forward_signals_to_process=False, restartable=True),
    )
    return TestDebugRequest(interactive_process)


@rule
async def setup_testng_debug_adapter_request(
    field_set: TestNGTestFieldSet,
) -> TestDebugAdapterRequest:
    raise NotImplementedError(
        "Debugging JUnit tests using a debug adapter has not yet been implemented."
    )


@rule
def generate_testng_lockfile_request(
    _: TestNGToolLockfileSentinel, testng: TestNG
) -> GenerateJvmLockfileFromTool:
    return GenerateJvmLockfileFromTool.create(testng)


def rules():
    return [
        *collect_rules(),
        *lockfile.rules(),
        UnionRule(TestFieldSet, TestNGTestFieldSet),
        UnionRule(GenerateToolLockfileSentinel, TestNGToolLockfileSentinel),
    ]
