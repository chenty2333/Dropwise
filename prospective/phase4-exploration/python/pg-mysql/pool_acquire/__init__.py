"""Pool-acquire cancellation cases (phase-4 exploration, round 3 follow-up).

Two units:

* ``psycopg_pool`` 3.3.3 ``AsyncConnectionPool`` -- cancel a *waiting*
  ``getconn()`` rather than a query.
* ``asyncpg`` 0.32.0 ``Pool`` -- cancel a *waiting* ``acquire()``.

The only shared code with the earlier round-3 cases is
``cases/common.py`` (bounded observation helpers and the retro engine path).
"""
