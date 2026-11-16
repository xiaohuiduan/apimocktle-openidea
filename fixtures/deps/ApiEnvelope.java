package com.example.api;

/** A dependency jar's wrapper type: the plugin never sees this source. */
public class ApiEnvelope<T> extends BaseReply {
    private Long errorNo;
    private String errorInfo;
    private T result;
}
