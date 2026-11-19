# Self-contained derivative of Sphinx's `test-ext-doctest` root.
# The only change is that the cleanup hook does not import the oracle's
# `tests.test_extensions` helper (which needs pytest); everything else —
# directive coverage, groups and expected failures — is preserved.
extensions = ['sphinx.ext.doctest']
project = 'self-contained doctest fixture'
root_doc = 'index'
exclude_patterns = ['_build']
doctest_global_setup = '''
CALLS = []
def record(kind, value):
    CALLS.append((kind, value))
    return value
'''
