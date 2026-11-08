package com.relate;

import com.fasterxml.jackson.annotation.JsonCreator;
import com.fasterxml.jackson.databind.JsonNode;

/**
 * One query notification (ADR-0363), as the ABI reports it: in the
 * statement's notification array and joined into a plan node's warnings.
 * In the statement's list, operator-scoped kinds carry the raising plan
 * operator's id and statement-scoped kinds leave
 * {@link #getOperatorId()} null; nested in a plan node's warnings the id
 * is always null — the plan projection deliberately drops the join key
 * the node itself already scopes.
 *
 * <p>{@code severity} and {@code category} stay open {@code String}s —
 * not closed enums like the error channel's
 * {@link RelateException.Stage} — as a stated decision: those two
 * vocabularies are shared surface metadata, and the cross-surface
 * alignment rule (every client surface gains a piece in the same change)
 * means closing them is a decision the Go, Swift and Java clients land
 * together, not one client unilaterally. The error stage closed exactly
 * that way, in the ABI v2 build.
 */
public final class Notification {
    private final String code;
    private final String gqlStatus;
    private final String severity;
    private final String category;
    private final String title;
    private final String description;
    private final String remedy;
    private final Long operatorId;

    // Decode by wire key, not by position: seven consecutive
    // String fields make a positional constructor a silent
    // field-swap hazard. Delegating creator: Jackson hands the
    // whole object node here when it appears nested in a plan.
    @JsonCreator(mode = JsonCreator.Mode.DELEGATING)
    private Notification(JsonNode node) {
        // One policy: the object guard and every key refusal below live in Json
        node = Json.requireObject(node, "notification entry", node.toString());
        // The refusal payload, serialized once — every key refusal below names it
        String payload = node.toString();
        this.code = Json.requireText(node, "code", "notification entry", payload);
        this.gqlStatus = Json.requireText(node, "gql_status", "notification entry", payload);
        this.severity = Json.requireText(node, "severity", "notification entry", payload);
        this.category = Json.requireText(node, "category", "notification entry", payload);
        this.title = Json.requireText(node, "title", "notification entry", payload);
        this.description = Json.requireText(node, "description", "notification entry", payload);
        this.remedy = Json.requireText(node, "remedy", "notification entry", payload);
        this.operatorId = Json.optionalLong(node, "operator_id", "notification entry", payload);
    }

    /** The notification kind's code, e.g. {@code Neo.ClientNotification.Statement.CartesianProduct}. */
    public String getCode() { return code; }
    /** The GQLSTATUS value (ADR-0363 decision 2), e.g. {@code 03N90}. */
    public String getGqlStatus() { return gqlStatus; }
    /** {@code INFORMATION} or {@code WARNING}. */
    public String getSeverity() { return severity; }
    /** {@code PERFORMANCE}, {@code UNRECOGNIZED}, {@code DEPRECATION}, {@code SCHEMA} or {@code HINT}. */
    public String getCategory() { return category; }
    /** The kind's short human-readable title, e.g. {@code Cartesian product}. */
    public String getTitle() { return title; }
    /** The instance diagnosis: the unknown name, index text, procedure name, ... */
    public String getDescription() { return description; }
    /** The action that clears the diagnostic. */
    public String getRemedy() { return remedy; }
    /**
     * The raising plan operator's id, or null: on statement-scoped kinds
     * in the statement's notification list, and on every entry nested in
     * a plan node's warnings (the plan projection drops the join key the
     * node itself already scopes).
     */
    public Long getOperatorId() { return operatorId; }
}
