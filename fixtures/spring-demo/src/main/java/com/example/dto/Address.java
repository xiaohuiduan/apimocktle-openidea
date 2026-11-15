package com.example.dto;

import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * 收件地址，被 {@link Order} 嵌套引用。
 */
public class Address {

    /**
     * 城市。
     */
    private String city;

    /**
     * 街道。
     */
    private String street;

    /**
     * 邮编。
     */
    @JsonProperty("zip_code")
    private String zipCode;

    public String getCity() {
        return city;
    }

    public String getStreet() {
        return street;
    }

    public String getZipCode() {
        return zipCode;
    }
}
