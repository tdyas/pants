# Copyright 2014 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

python_tests(
  sources=['test_sitegen.py'],
  dependencies=[
    'pants-plugins/3rdparty/python:beautifulsoup4',
    'pants-plugins/src/python/internal_backend/sitegen/tasks:sitegen',
    'src/python/pants/engine/internals:native',
  ],
  tags = {"partially_type_checked"},
)
