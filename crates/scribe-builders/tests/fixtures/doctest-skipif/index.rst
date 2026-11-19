Skipif fixture
==============

Skipped setup
-------------

.. testsetup:: group
   :skipif: record('testsetup', 'skip') == 'recorded skip'

   record('setup body', 'should not run')

.. testsetup:: group
   :skipif: record('testsetup', 'run') == 'recorded run'

   record('setup body', 'should run')

Executed and skipped doctests
-----------------------------

.. doctest:: group
   :skipif: record('doctest', 'skip') == 'recorded skip'

   >>> 1 / 0
   ZeroDivisionError: ignored because skipped

.. doctest:: group

   >>> print(record('doctest', 'run'))
   recorded run

testcode with skipif
--------------------

.. testcode:: group
   :skipif: record('testcode', 'skip') == 'recorded skip'

   print('ignored')

.. testoutput:: group
   :skipif: record('testoutput', 'skip') == 'recorded skip'

   ignored

.. testcode:: group
   :skipif: record('testcode', 'run') == 'recorded run'

   print(record('testcode body', 'run'))

.. testoutput:: group
   :skipif: record('testoutput', 'run') == 'recorded run'

   recorded run
