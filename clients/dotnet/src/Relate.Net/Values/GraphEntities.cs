using System.Diagnostics.CodeAnalysis;

namespace Relate.Net;

/// <summary>
/// The shared shape of a node and a relationship: element identity plus properties, with property
/// access and equality by <see cref="GraphEntity.ElementId"/>.
/// </summary>
public abstract class GraphEntity
{
    /// <summary>Creates an entity. Normally created by the library when reading results.</summary>
    private protected GraphEntity(string elementId, long id, IReadOnlyDictionary<string, object?> properties)
    {
        ElementId = elementId ?? throw new ArgumentNullException(nameof(elementId));
        Id = id;
        Properties = properties ?? throw new ArgumentNullException(nameof(properties));
    }

    /// <summary>The element id, exactly as Cypher <c>elementId(n)</c> returns it.</summary>
    public string ElementId { get; }

    /// <summary>The (deprecated) numeric id, exactly as Cypher <c>id(n)</c> returns it.</summary>
    public long Id { get; }

    /// <summary>The entity's properties, in engine order.</summary>
    public IReadOnlyDictionary<string, object?> Properties { get; }

    /// <summary>Gets a property value; throws <see cref="KeyNotFoundException"/> when absent.</summary>
    public object? this[string key] => Properties[key];

    /// <summary>Gets a property converted to <typeparamref name="T"/>; see <see cref="RelateRecord.Get{T}(string)"/>.</summary>
    [return: MaybeNull]
    public T Get<T>(string key) => ValueConverter.Convert<T>(Properties[key]);

    /// <summary>
    /// Equal when both are the same derived kind with the same <see cref="GraphEntity.ElementId"/>.
    /// </summary>
    public override bool Equals(object? obj) =>
        obj is GraphEntity other
        && GetType() == other.GetType()
        && string.Equals(ElementId, other.ElementId, StringComparison.Ordinal);

    /// <inheritdoc />
    public override int GetHashCode() => StringComparer.Ordinal.GetHashCode(ElementId);
}

/// <summary>A graph node returned by a query.</summary>
/// <remarks>Two nodes are equal when they have the same <see cref="GraphEntity.ElementId"/>.</remarks>
public sealed class Node : GraphEntity, IEquatable<Node>
{
    /// <summary>Creates a node. Normally created by the library when reading results.</summary>
    public Node(string elementId, long id, IReadOnlyList<string> labels, IReadOnlyDictionary<string, object?> properties)
        : base(elementId, id, properties)
    {
        Labels = labels ?? throw new ArgumentNullException(nameof(labels));
    }

    /// <summary>The node's labels.</summary>
    public IReadOnlyList<string> Labels { get; }

    /// <summary>Returns <see langword="true"/> when the node carries <paramref name="label"/>.</summary>
    public bool HasLabel(string label) => Labels.Contains(label, StringComparer.Ordinal);

    /// <inheritdoc />
    public bool Equals(Node? other) => base.Equals(other);

    /// <inheritdoc />
    public override string ToString() => $"({string.Concat(Labels.Select(l => ":" + l))} {ValueFormatter.Format(Properties)})";
}

/// <summary>A graph relationship returned by a query.</summary>
/// <remarks>Two relationships are equal when they have the same <see cref="GraphEntity.ElementId"/>.</remarks>
public sealed class Relationship : GraphEntity, IEquatable<Relationship>
{
    /// <summary>Creates a relationship. Normally created by the library when reading results.</summary>
    public Relationship(
        string elementId,
        long id,
        string type,
        string startNodeElementId,
        long startNodeId,
        string endNodeElementId,
        long endNodeId,
        IReadOnlyDictionary<string, object?> properties)
        : base(elementId, id, properties)
    {
        Type = type ?? throw new ArgumentNullException(nameof(type));
        StartNodeElementId = startNodeElementId ?? throw new ArgumentNullException(nameof(startNodeElementId));
        StartNodeId = startNodeId;
        EndNodeElementId = endNodeElementId ?? throw new ArgumentNullException(nameof(endNodeElementId));
        EndNodeId = endNodeId;
    }

    /// <summary>The relationship type.</summary>
    public string Type { get; }

    /// <summary>The element id of the start node.</summary>
    public string StartNodeElementId { get; }

    /// <summary>The numeric id of the start node.</summary>
    public long StartNodeId { get; }

    /// <summary>The element id of the end node.</summary>
    public string EndNodeElementId { get; }

    /// <summary>The numeric id of the end node.</summary>
    public long EndNodeId { get; }

    /// <inheritdoc />
    public bool Equals(Relationship? other) => base.Equals(other);

    /// <inheritdoc />
    public override string ToString() => $"[:{Type} {ValueFormatter.Format(Properties)}]";
}

/// <summary>A path: an alternating sequence of nodes and relationships.</summary>
public sealed class GraphPath
{
    /// <summary>Creates a path. <paramref name="nodes"/> must have exactly one more element than <paramref name="relationships"/>.</summary>
    public GraphPath(IReadOnlyList<Node> nodes, IReadOnlyList<Relationship> relationships)
    {
        ArgumentNullException.ThrowIfNull(nodes);
        ArgumentNullException.ThrowIfNull(relationships);
        if (nodes.Count != relationships.Count + 1)
        {
            throw new ArgumentException("A path has exactly one more node than relationships.", nameof(nodes));
        }

        Nodes = nodes;
        Relationships = relationships;
    }

    /// <summary>The nodes along the path, from start to end.</summary>
    public IReadOnlyList<Node> Nodes { get; }

    /// <summary>The relationships along the path, in traversal order.</summary>
    public IReadOnlyList<Relationship> Relationships { get; }

    /// <summary>The first node.</summary>
    public Node Start => Nodes[0];

    /// <summary>The last node.</summary>
    public Node End => Nodes[^1];

    /// <summary>The number of hops (relationships).</summary>
    public int Length => Relationships.Count;

    /// <inheritdoc />
    public override string ToString() =>
        string.Concat(Nodes.Select((n, i) => i < Relationships.Count ? $"{n}-{Relationships[i]}->" : n.ToString()));
}
