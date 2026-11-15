package com.example.dto;

import com.fasterxml.jackson.annotation.JsonIgnore;
import com.fasterxml.jackson.annotation.JsonProperty;

import java.math.BigDecimal;

/**
 * 订单。
 */
public class Order {

    /**
     * 单次查询的上限；全大写常量不参与序列化。
     */
    public static final int MAX_ITEMS = 200;

    /**
     * 订单号，对外字段名由 {@code @JsonProperty} 改名。
     */
    @JsonProperty("order_no")
    private String orderNo;

    /**
     * 金额。
     */
    private BigDecimal amount;

    /**
     * 收件地址。
     */
    private Address address;

    /**
     * 内部追踪号，不对外暴露。
     */
    @JsonIgnore
    private String internalTrace;

    public String getOrderNo() {
        return orderNo;
    }

    public BigDecimal getAmount() {
        return amount;
    }

    public Address getAddress() {
        return address;
    }

    public String getInternalTrace() {
        return internalTrace;
    }
}
