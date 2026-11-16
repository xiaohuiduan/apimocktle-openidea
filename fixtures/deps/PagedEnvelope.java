package com.example.api;

import java.io.Serializable;
import java.util.List;

/**
 * Both shapes javac writes after the type parameters: a generic superclass with
 * its own type arguments, then one or more interfaces. A reader that stops at
 * the superclass and demands end-of-input throws the entire signature away.
 */
public class PagedEnvelope<T> extends GenericBase<List<T>> implements Serializable, Cloneable {
    private static final long serialVersionUID = 1L;

    private T items;
}
