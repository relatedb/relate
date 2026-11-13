"""The two walks over the row-value vocabulary (ADR-0213, ADR-0214).

``Record.data()``/``to_dict()`` rebuild a row with :func:`map_row_value` and
``collect_graph_entities`` reads one with :func:`iter_graph_entities`. They
share the sequence view (:func:`_children`), not one leaf table:
``map_row_value`` treats every ``_GRAPH_VALUES`` member as a leaf and rebuilds
a mapping from its keys, while ``iter_graph_entities`` flattens a ``Path`` and
reads a mapping's values. A new container type therefore has one home — the
sequence view — and each walk keeps its own leaf policy.
"""

from typing import Any, Callable, Iterable, Iterator, Mapping, Optional, Union

from relate._native import Node, Path, Relationship

# The entity kinds a walk hands to its callback, and the graph values the
# rebuild walk treats as leaves (an entity or a whole path).
_ENTITY_TYPES = (Node, Relationship)
_GRAPH_VALUES = (*_ENTITY_TYPES, Path)

# Sequence and set types that recursively contain graph values (mirrors
# neo4j_viz.neo4j.find_graph_entity, which walks lists and dicts); any
# Mapping, dict or not, is walked ahead of these.
_SEQUENCE_TYPES = (list, tuple, set, frozenset)


def _children(value: Any) -> Optional[Iterable[Any]]:
    """The values nested in a sequence, as its own iterable view, or ``None``
    for anything else. The one sequence view both walks share; each reads a
    mapping its own way — the rebuild walk needs the keys, the entity walk the
    values."""
    if isinstance(value, _SEQUENCE_TYPES):
        return value
    return None


def map_row_value(value: Any, on_graph_value: Callable[[Any], Any]) -> Any:
    """Rebuild ``value`` with each graph value replaced by the callback's result.

    A ``Mapping`` becomes a ``dict``, a list, tuple, set or frozenset a
    ``list``, each rebuilt recursively; anything else is returned as is.
    """
    if isinstance(value, _GRAPH_VALUES):
        return on_graph_value(value)
    if isinstance(value, Mapping):
        return {k: map_row_value(v, on_graph_value) for k, v in value.items()}
    children = _children(value)
    if children is None:
        return value
    return [map_row_value(v, on_graph_value) for v in children]


def iter_graph_entities(value: Any) -> Iterator[Union[Node, Relationship]]:
    """Yield each ``Node`` and ``Relationship`` nested in ``value``.

    The one walk over the graph-value vocabulary: a ``Path`` is flattened into
    its own nodes and relationships, so a caller never handles a ``Path``
    itself and only ever sees the two entity kinds.
    """
    if isinstance(value, Path):
        yield from value.nodes
        yield from value.relationships
        return
    if isinstance(value, _ENTITY_TYPES):
        yield value
        return
    if isinstance(value, Mapping):
        for child in value.values():
            yield from iter_graph_entities(child)
        return
    children = _children(value)
    if children is not None:
        for child in children:
            yield from iter_graph_entities(child)
