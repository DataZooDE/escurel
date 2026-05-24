---
type: instance
skill: customer
id: broken
this is: not valid
  yaml: at all
    : : :
---

# Broken page

Frontmatter above is malformed YAML on purpose; the parser must
return a `ParseError::Yaml` for this fixture.
