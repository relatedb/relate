"""The one walk over the row-value vocabulary (ADR-0213, ADR-0214).

``Record.data()``/``to_dict()`` rebuild a row with :func:`map_row_value`;
``collect_graph_entities`` reads one with :func:`iter_graph_values`. Both share
the same leaf and container vocabulary, so a new container type is handled
the same way in both.
"""

from typing import Any, Callable, Iterator, Mapping

from relate._native import Node, Path, Relationship

# Leaf types the walks hand to their callback.
_GRAPH_VALUES = (Node, Relationship, Path)

# Sequence and set types that recursively contain graph values (mirrors
# neo4j_viz.neo4j.find_graph_entity, which walks lists and dicts); any
# Mapping, dict or not, is walked ahead of these.
_SEQUENCE_TYPES = (list, tuple, set, frozenset)


def _children(value: Any):
    """The values nested in a container, or ``None`` for a leaf."""
    if isinstance(value, Mapping):
        return list(value.values())
    if isinstance(value, _SEQUENCE_TYPES):
        return list(value)
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


def iter_graph_values(value: Any) -> Iterator[Any]:
    """Yield each graph value nested in ``value``, building nothing."""
    if isinstance(value, _GRAPH_VALUES):
        yield value
        return
    for child in _children(value) or ():
        yield from iter_graph_values(child)
