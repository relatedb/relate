### Changed

- Two parameter-name spellings Neo4j accepts now plan and run: the
  backtick-quoted name (``$`p1``, ``$`p 1``) and whitespace between `$`
  and the name (`$ p2`) — both previously failed at the lexer with
  `InvalidParameterName`. Every `$name` render now goes through one
  canonical quoting policy, so two different parameter names can never
  collide in a plan-cache key; `$ €`, a bare `$` and `$ (x)` keep their
  rejections.
