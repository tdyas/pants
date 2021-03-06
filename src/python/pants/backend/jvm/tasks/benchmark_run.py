# Copyright 2014 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

import os
import shutil

from pants.backend.jvm.targets.benchmark import Benchmark
from pants.backend.jvm.tasks.jvm_task import JvmTask
from pants.backend.jvm.tasks.jvm_tool_task_mixin import JvmToolTaskMixin
from pants.base.exceptions import TaskError
from pants.base.workunit import WorkUnitLabel
from pants.java.distribution.distribution import DistributionLocator
from pants.java.executor import SubprocessExecutor
from pants.java.jar.jar_dependency import JarDependency
from pants.java.util import execute_java


class BenchmarkRun(JvmToolTaskMixin, JvmTask):
    _CALIPER_MAIN = "com.google.caliper.Runner"

    @classmethod
    def register_options(cls, register):
        super().register_options(register)
        register("--target", help="Name of the benchmark class. This is a mandatory argument.")
        register("--memory", type=bool, help="Enable memory profiling.")
        register("--debug", type=bool, help="Run the benchmark tool with in process debugging.")

        cls.register_jvm_tool(
            register,
            "benchmark-tool",
            classpath=[
                # TODO (Eric Ayers) Caliper is old. Add jmh support?
                # The caliper tool is shaded, and so shouldn't interfere with Guava 16.
                JarDependency(org="com.google.caliper", name="caliper", rev="0.5-rc1"),
            ],
            classpath_spec="//:benchmark-caliper-0.5",
            main=cls._CALIPER_MAIN,
        )
        cls.register_jvm_tool(
            register,
            "benchmark-agent",
            classpath=[
                JarDependency(
                    org="com.google.code.java-allocation-instrumenter",
                    name="java-allocation-instrumenter",
                    rev="2.1",
                    intransitive=True,
                ),
            ],
            classpath_spec="//:benchmark-java-allocation-instrumenter-2.1",
        )

    @classmethod
    def subsystem_dependencies(cls):
        return super().subsystem_dependencies() + (DistributionLocator,)

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        # TODO(Steve Gury):
        # Find all the target classes from the Benchmark target itself
        # https://jira.twitter.biz/browse/AWESOME-1938
        if not self.get_options().target:
            raise ValueError("Mandatory argument --target must be specified.")
        self.args.insert(0, self.get_options().target)
        if self.get_options().memory:
            self.args.append("--measureMemory")
        if self.get_options().debug:
            self.args.append("--debug")

    def _is_benchmark(self, target):
        return isinstance(target, Benchmark)

    def execute(self):
        targets = self.context.targets(predicate=self._is_benchmark)
        if not targets:
            raise TaskError("No jvm targets specified for benchmarking.")

        # For rewriting JDK classes to work, the JAR file has to be listed specifically in
        # the JAR manifest as something that goes in the bootclasspath.
        # The MANIFEST list a jar 'allocation.jar' this is why we have to rename it
        agent_tools_classpath = self.tool_classpath("benchmark-agent")
        agent_jar = agent_tools_classpath[0]
        allocation_jar = os.path.join(os.path.dirname(agent_jar), "allocation.jar")

        # TODO(Steve Gury): Find a solution to avoid copying the jar every run and being resilient
        # to version upgrade
        shutil.copyfile(agent_jar, allocation_jar)
        os.environ["ALLOCATION_JAR"] = str(allocation_jar)

        benchmark_tools_classpath = self.tool_classpath("benchmark-tool")

        # Collect a transitive classpath for the benchmark targets.
        classpath = self.classpath(targets, benchmark_tools_classpath)

        java_executor = SubprocessExecutor(self.preferred_jvm_distribution_for_targets(targets))
        exit_code = execute_java(
            classpath=classpath,
            main=self._CALIPER_MAIN,
            jvm_options=self.jvm_options,
            args=self.args,
            workunit_factory=self.context.new_workunit,
            workunit_name="caliper",
            workunit_labels=[WorkUnitLabel.RUN],
            executor=java_executor,
            create_synthetic_jar=self.synthetic_classpath,
        )
        if exit_code != 0:
            raise TaskError(f"java {self._CALIPER_MAIN} ... exited non-zero ({exit_code})")
