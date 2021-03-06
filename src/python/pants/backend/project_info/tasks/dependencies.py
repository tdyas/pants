# Copyright 2014 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from pants.backend.jvm.targets.jar_library import JarLibrary
from pants.backend.jvm.targets.jvm_app import JvmApp
from pants.backend.jvm.targets.jvm_target import JvmTarget
from pants.backend.project_info.rules.dependencies import DependencyType
from pants.base.payload_field import JarsField, PythonRequirementsField
from pants.task.console_task import ConsoleTask
from pants.util.ordered_set import OrderedSet


class Dependencies(ConsoleTask):
    """Print the target's dependencies."""

    @staticmethod
    def _is_jvm(target):
        return isinstance(target, (JarLibrary, JvmTarget, JvmApp))

    @classmethod
    def register_options(cls, register):
        super().register_options(register)
        register(
            "--type",
            type=DependencyType,
            default=DependencyType.SOURCE,
            help="Which types of dependencies to find, where `source` means source code dependencies "
            "and `3rdparty` means third-party requirements and JARs.",
        )

    def console_output(self, unused_method_argument):
        ordered_closure = OrderedSet()
        for target in self.context.target_roots:
            if self.act_transitively:
                target.walk(ordered_closure.add)
            else:
                ordered_closure.update(target.dependencies)

        include_source = self.get_options().type in [
            DependencyType.SOURCE,
            DependencyType.SOURCE_AND_THIRD_PARTY,
        ]
        include_3rdparty = self.get_options().type in [
            DependencyType.THIRD_PARTY,
            DependencyType.SOURCE_AND_THIRD_PARTY,
        ]
        for tgt in ordered_closure:
            if include_source:
                yield tgt.address.spec
            if include_3rdparty:
                # TODO(John Sirois): We need an external payload abstraction at which point knowledge
                # of jar and requirement payloads can go and this hairball will be untangled.
                if isinstance(tgt.payload.get_field("requirements"), PythonRequirementsField):
                    for requirement in tgt.payload.requirements:
                        yield str(requirement.requirement)
                elif isinstance(tgt.payload.get_field("jars"), JarsField):
                    for jar in tgt.payload.jars:
                        data = dict(org=jar.org, name=jar.name, rev=jar.rev)
                        yield ("{org}:{name}:{rev}" if jar.rev else "{org}:{name}").format(**data)
