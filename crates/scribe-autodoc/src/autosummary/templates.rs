//! Bundled autosummary stub templates, vendored verbatim from the pinned
//! Sphinx oracle (`sphinx/sphinx/ext/autosummary/templates/autosummary/`).
//!
//! See `crates/scribe-autodoc/src/autosummary/NOTICE` for attribution and
//! the upstream license. The content is rendered with minijinja; the Jinja2
//! i18n `{{ _("...") }}` calls are identity functions in the default English
//! locale, which is what Sphinx does without a translation catalog.

/// Verbatim oracle template `base.rst`.
pub const BASE_TEMPLATE: &str = r####"{{ fullname | escape | underline}}

.. currentmodule:: {{ module }}

.. auto{{ objtype }}:: {{ objname }}
"####;

/// Verbatim oracle template `module.rst`.
pub const MODULE_TEMPLATE: &str = r####"{{ fullname | escape | underline}}

.. automodule:: {{ fullname }}

   {% block attributes %}
   {%- if attributes %}
   .. rubric:: {{ _('Module Attributes') }}

   .. autosummary::
   {% for item in attributes %}
      {{ item }}
   {%- endfor %}
   {% endif %}
   {%- endblock %}

   {%- block functions %}
   {%- if functions %}
   .. rubric:: {{ _('Functions') }}

   .. autosummary::
   {% for item in functions %}
      {{ item }}
   {%- endfor %}
   {% endif %}
   {%- endblock %}

   {%- block classes %}
   {%- if classes %}
   .. rubric:: {{ _('Classes') }}

   .. autosummary::
   {% for item in classes %}
      {{ item }}
   {%- endfor %}
   {% endif %}
   {%- endblock %}

   {%- block exceptions %}
   {%- if exceptions %}
   .. rubric:: {{ _('Exceptions') }}

   .. autosummary::
   {% for item in exceptions %}
      {{ item }}
   {%- endfor %}
   {% endif %}
   {%- endblock %}

{%- block modules %}
{%- if modules %}
.. rubric:: Modules

.. autosummary::
   :toctree:
   :recursive:
{% for item in modules %}
   {{ item }}
{%- endfor %}
{% endif %}
{%- endblock %}
"####;

/// Verbatim oracle template `class.rst`.
pub const CLASS_TEMPLATE: &str = r####"{{ fullname | escape | underline}}

.. currentmodule:: {{ module }}

.. autoclass:: {{ objname }}

   {% block methods %}
   .. automethod:: __init__

   {% if methods %}
   .. rubric:: {{ _('Methods') }}

   .. autosummary::
   {% for item in methods %}
      ~{{ name }}.{{ item }}
   {%- endfor %}
   {% endif %}
   {% endblock %}

   {% block attributes %}
   {% if attributes %}
   .. rubric:: {{ _('Attributes') }}

   .. autosummary::
   {% for item in attributes %}
      ~{{ name }}.{{ item }}
   {%- endfor %}
   {% endif %}
   {% endblock %}
"####;

