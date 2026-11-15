package com.example.dto;

/**
 * 统一返回包装。
 *
 * @param <T> 有效载荷类型
 */
public class Result<T> {

    /**
     * 业务状态码。
     */
    private int code;

    /**
     * 提示信息。
     */
    private String msg;

    /**
     * 有效载荷。
     */
    private T data;

    public static <T> Result<T> ok(T data) {
        Result<T> result = new Result<>();
        result.code = 0;
        result.data = data;
        return result;
    }

    public int getCode() {
        return code;
    }

    public String getMsg() {
        return msg;
    }

    public T getData() {
        return data;
    }
}
