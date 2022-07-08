# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

import re
from textwrap import dedent

import pytest

from internal_plugins.test_lockfile_fixtures.lockfile_fixture import (
    JVMLockfileFixture,
    JVMLockfileFixtureDefinition,
)
from pants.backend.java.compile.javac import rules as javac_rules
from pants.backend.java.target_types import JavaSourcesGeneratorTarget, TestNGTestsGeneratorTarget
from pants.backend.java.target_types import rules as target_types_rules
from pants.backend.scala.compile.scalac import rules as scalac_rules
from pants.backend.scala.target_types import rules as scala_target_types_rules
from pants.build_graph.address import Address
from pants.core.goals.test import TestResult
from pants.core.target_types import FilesGeneratorTarget, FileTarget, RelocatedFiles
from pants.core.util_rules import config_files, source_files
from pants.core.util_rules.external_tool import rules as external_tool_rules
from pants.engine.addresses import Addresses
from pants.engine.target import CoarsenedTargets
from pants.jvm import classpath
from pants.jvm.jdk_rules import rules as java_util_rules
from pants.jvm.non_jvm_dependencies import rules as non_jvm_dependencies_rules
from pants.jvm.resolve.coursier_fetch import rules as coursier_fetch_rules
from pants.jvm.resolve.coursier_setup import rules as coursier_setup_rules
from pants.jvm.target_types import JvmArtifactTarget
from pants.jvm.test.testng import TestNGTestFieldSet
from pants.jvm.test.testng import rules as testng_rules
from pants.jvm.util_rules import rules as util_rules
from pants.testutil.rule_runner import PYTHON_BOOTSTRAP_ENV, QueryRule, RuleRunner

# TODO(12812): Switch tests to using parsed junit.xml results instead of scanning stdout strings.


@pytest.fixture
def rule_runner() -> RuleRunner:
    rule_runner = RuleRunner(
        preserve_tmpdirs=True,
        rules=[
            *classpath.rules(),
            *config_files.rules(),
            *coursier_fetch_rules(),
            *coursier_setup_rules(),
            *external_tool_rules(),
            *java_util_rules(),
            *javac_rules(),
            *testng_rules(),
            *scala_target_types_rules(),
            *scalac_rules(),
            *source_files.rules(),
            *target_types_rules(),
            *util_rules(),
            *non_jvm_dependencies_rules(),
            QueryRule(CoarsenedTargets, (Addresses,)),
            QueryRule(TestResult, (TestNGTestFieldSet,)),
        ],
        target_types=[
            FileTarget,
            FilesGeneratorTarget,
            RelocatedFiles,
            JvmArtifactTarget,
            JavaSourcesGeneratorTarget,
            TestNGTestsGeneratorTarget,
        ],
    )
    rule_runner.set_options(
        args=[],
        env_inherit=PYTHON_BOOTSTRAP_ENV,
    )
    return rule_runner


@pytest.fixture
def testng_lockfile_def() -> JVMLockfileFixtureDefinition:
    return JVMLockfileFixtureDefinition(
        "testng.test.lock",
        ["org.testng:testng:7.6.0"],
    )


@pytest.fixture
def testng_lockfile(
    testng_lockfile_def: JVMLockfileFixtureDefinition, request
) -> JVMLockfileFixture:
    return testng_lockfile_def.load(request)


def run_testng_test(
    rule_runner: RuleRunner, target_name: str, relative_file_path: str
) -> TestResult:
    tgt = rule_runner.get_target(
        Address(spec_path="", target_name=target_name, relative_file_path=relative_file_path)
    )
    return rule_runner.request(TestResult, [TestNGTestFieldSet.create(tgt)])


def test_testng_simple_success(
    rule_runner: RuleRunner, testng_lockfile: JVMLockfileFixture
) -> None:
    rule_runner.write_files(
        {
            "3rdparty/jvm/default.lock": testng_lockfile.serialized_lockfile,
            "3rdparty/jvm/BUILD": testng_lockfile.requirements_as_jvm_artifact_targets(),
            "BUILD": dedent(
                """\
                testng_tests(
                    name='example-test',
                    dependencies= [
                        '3rdparty/jvm:org.testng_testng',
                    ],
                )
                """
            ),
            "SimpleTest.java": dedent(
                """
                package org.pantsbuild.example;

                import org.testng.annotations.Test;
                import static org.testng.AssertJUnit.*;

                public class SimpleTest {
                    @Test
                    public void testHello(){
                        assertTrue("Hello!" == "Hello!");
                    }
                }
                """
            ),
        }
    )

    test_result = run_testng_test(rule_runner, "example-test", "SimpleTest.java")
    print(f"RESULT:\nstdout:\n{test_result.stdout}\n\nstderr:\n{test_result.stderr}\n")

    assert test_result.exit_code == 0
    assert re.search(r"Finished:\s+testHello", test_result.stdout) is not None
    assert re.search(r"1 tests successful", test_result.stdout) is not None
    assert re.search(r"1 tests found", test_result.stdout) is not None
