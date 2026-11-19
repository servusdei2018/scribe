Self-contained doctest fixture
==============================

Free-standing blocks
--------------------

>>> 1 + 1
2
>>> print('hello')
hello

Directives
----------

.. doctest::

   >>> 2 + 2
   4

.. testcode:: group1

   x = 5
   print(x + 1)

.. testoutput:: group1

   6

Setup and cleanup
-----------------

.. testsetup:: *

   def squared(x):
       return x * x

.. doctest::

   >>> squared(3)
   9

.. testcode::

   print(squared(4))

.. testoutput::

   16

.. testcleanup:: *

   record('cleanup', 'done')

Options and ellipsis
--------------------

.. doctest::

   >>> print('a', 'b', 'c')
   a b c
   >>> 'abcdef'  # doctest: +ELLIPSIS
   'a...f'

.. testcode::
   :hide:

   print('Output         text.')

.. testoutput::
   :hide:
   :options: +NORMALIZE_WHITESPACE

   Output text.

.. doctest::
   :pyversion: >= 2.0

   >>> 10 - 3
   7

.. doctest::
   :pyversion: < 2.0

   >>> 1 + 1
   999

Groups
------

.. testsetup:: group2

   def add(x, y):
       return x + y

.. doctest:: group2

   >>> add(2, 3)
   5

.. doctest:: group3

   >>> add(2, 3)
   Traceback (most recent call last):
     ...
   NameError: name 'add' is not defined

Exceptions and non-ASCII
------------------------

.. doctest::

   >>> 1 / 0
   Traceback (most recent call last):
     ...
   ZeroDivisionError: division by zero
   >>> print('umlauts: äöü.')
   umlauts: äöü.

SKIP flag
---------

.. doctest::

   >>> 1 / 0  # doctest: +SKIP
   ZeroDivisionError: deliberately not run
