"""Build the canonical AST fixture and print Docutils pseudo-XML.

This is the oracle used by ``tests/pseudoxml.rs``. It builds exactly the tree
that ``scribe_ast::Document::fixture_document`` builds and prints
``document.pformat()`` -- the very code path that
``sphinx-build -b pseudoxml`` ends in (``sphinx/writers/xml.py``:
``PseudoXMLWriter.translate``).

Run with:

    /home/credo/code/scribe/.oracle-venv/bin/python \\
        crates/scribe-ast/tests/fixtures/gen_pseudoxml.py
"""

from docutils import nodes
from sphinx import addnodes
from sphinx.util.docutils import new_document

doc = new_document('test.rst')
section = nodes.section(ids=['a-section'], names=['a section'])
doc += section

section += nodes.title(text='A Section')

p = nodes.paragraph()
section += p
p += nodes.Text('See ')
xr = addnodes.pending_xref(
    '',
    refdomain='py',
    reftype='func',
    reftarget='io.StringIO',
    refexplicit=False,
    refspecific=False,
    refwarn=True,
    refdoc='index',
    refid='',
    refuri='',
    reftitle='',
    refname='StringIO',
    classes=['xref', 'py', 'func'],
)
xr += nodes.literal(text='StringIO')
p += xr
p += nodes.Text(' done.')

p = nodes.paragraph()
section += p
p += nodes.Text('This is ')
p += nodes.emphasis(text='emphasis')
p += nodes.Text(' and ')
p += nodes.literal(text='literal')
p += nodes.Text(' and ')
p += nodes.strong(text='strong')
p += nodes.Text('.')

bullet = nodes.bullet_list(bullet='-')
section += bullet
for text in ('first item', 'second item'):
    item = nodes.list_item()
    bullet += item
    item += nodes.paragraph(text=text)

note = nodes.note()
section += note
note += nodes.paragraph(text='This is a note admonition.')

print(doc.pformat(), end='')
