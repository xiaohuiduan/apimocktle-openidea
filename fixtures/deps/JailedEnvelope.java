package com.example.api;

import java.io.Serializable;

/**
 * A dependency jar's wrapper type that also implements an interface: javac then
 * writes the interface after the superclass in the class Signature, which is
 * where the reader used to give up and lose {@code T}.
 */
public class JailedEnvelope<T> extends BaseReply implements Serializable {
    private static final long serialVersionUID = 1L;

    private T payload;
}
