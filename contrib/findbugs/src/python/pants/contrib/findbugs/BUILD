# Copyright 2016 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

contrib_plugin(
  name='plugin',
  dependencies=[
    'contrib/findbugs/src/python/pants/contrib/findbugs/tasks',
    'src/python/pants/build_graph',
    'src/python/pants/goal:task_registrar',
  ],
  distribution_name='pantsbuild.pants.contrib.findbugs',
  description='FindBugs pants plugin (deprecated)',
  register_goals=True,
  tags = {"partially_type_checked"},
)
