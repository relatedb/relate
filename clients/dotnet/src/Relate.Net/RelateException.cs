using System.Text.Json;
using Relate.Net.Interop;

namespace Relate.Net;

/// <summary>The engine stage at which a relate operation failed.</summary>
public enum ErrorStage
{
    /// <summary>The stage was not reported.</summary>
    Unknown = 0,

    /// <summary>The statement failed to parse, bind or plan (for example a syntax error).</summary>
    Compile = 1,

    /// <summary>The statement failed while executing (for example a division by zero).</summary>
    Runtime = 2,

    /// <summary>
    /// The failure happened outside the query engine: an invalid argument or handle, an IO or
    /// storage failure, or an internal engine error.
    /// </summary>
    Internal = 3,
}

/// <summary>
/// The exception thrown when relate reports a failure. It carries the Neo4j-compatible status
/// <see cref="Code"/>, the ISO GQL <see cref="GqlStatus"/>, the failing <see cref="Stage"/> and
/// whether the operation <see cref="IsRetryable">may be retried</see>.
/// </summary>
public class RelateException : Exception
{
    /// <summary>Creates an exception with a default message.</summary>
    public RelateException()
        : this("A relate operation failed.")
    {
    }

    /// <summary>Creates an exception with the given message.</summary>
    public RelateException(string message)
        : this(message, innerException: null)
    {
    }

    /// <summary>Creates an exception with the given message and inner exception.</summary>
    public RelateException(string message, Exception? innerException)
        : this(message, code: string.Empty, gqlStatus: string.Empty, ErrorStage.Unknown, isRetryable: false, innerException)
    {
    }

    /// <summary>Creates an exception with full relate error details.</summary>
    public RelateException(string message, string code, string gqlStatus, ErrorStage stage, bool isRetryable)
        : this(message, code, gqlStatus, stage, isRetryable, innerException: null)
    {
    }

    /// <summary>Creates an exception with full relate error details and an inner exception.</summary>
    public RelateException(string message, string code, string gqlStatus, ErrorStage stage, bool isRetryable, Exception? innerException)
        : base(message, innerException)
    {
        Code = code;
        GqlStatus = gqlStatus;
        Stage = stage;
        IsRetryable = isRetryable;
    }

    /// <summary>The Neo4j-compatible status code, e.g. <c>Neo.ClientError.Statement.SyntaxError</c>.</summary>
    public string Code { get; }

    /// <summary>The ISO GQL status code, e.g. <c>42001</c>.</summary>
    public string GqlStatus { get; }

    /// <summary>The engine stage at which the failure happened.</summary>
    public ErrorStage Stage { get; }

    /// <summary><see langword="true"/> when retrying the same operation may succeed (for example a transient conflict).</summary>
    public bool IsRetryable { get; }

    /// <inheritdoc />
    public override string ToString() =>
        string.IsNullOrEmpty(Code) ? base.ToString() : $"{GetType().FullName} [{Code} / {GqlStatus}]: {base.ToString()}";

    /// <summary>
    /// Decodes the engine's error document. Every field is required (the C ABI guarantees the
    /// shape on every error result); a missing field or malformed document is contract drift and
    /// fails loudly naming the field, the way the Go client's error decode does.
    /// </summary>
    /// <exception cref="FormatException">The document is absent, malformed, or lacks a required field.</exception>
    internal static RelateException FromErrorJson(string? json, string operation)
    {
        if (string.IsNullOrWhiteSpace(json))
        {
            throw new FormatException($"The relate operation '{operation}' failed without an error document.");
        }

        JsonElement root = JsonValues.ParseDocument(json, Document(operation));

        return new RelateException(
            JsonValues.RequiredString(root, "message", Document(operation)),
            JsonValues.RequiredString(root, "code", Document(operation)),
            JsonValues.RequiredString(root, "gql_status", Document(operation)),
            JsonValues.RequiredString(root, "stage", Document(operation)) switch
            {
                "compile" => ErrorStage.Compile,
                "runtime" => ErrorStage.Runtime,
                "internal" => ErrorStage.Internal,
                string other => throw new FormatException($"The {Document(operation)} has an unknown stage '{other}'."),
            },
            JsonValues.RequiredBoolean(root, "retryable", Document(operation)));
    }

    private static string Document(string operation) => $"error document from '{operation}'";
}
