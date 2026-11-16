package com.example.api;

import java.util.List;

/**
 * A generic base type for the jar: the point is that a subclass' class
 * Signature carries type arguments on both sides of it.
 */
public class GenericBase<C> {
    private Integer code;

    private C content;
}
