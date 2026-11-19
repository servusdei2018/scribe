# Self-contained derivative of Sphinx's `test-ext-doctest-skipif` root:
# the record() helper is defined here instead of importing pytest-backed tests.
extensions = ['sphinx.ext.doctest']
project = 'self-contained doctest skipif fixture'
root_doc = 'index'
exclude_patterns = ['_build']
doctest_global_setup = '''
LOG = []
def record(kind, value):
    LOG.append((kind, value))
    return 'recorded ' + str(value)
'''
