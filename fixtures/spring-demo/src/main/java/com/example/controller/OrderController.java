package com.example.controller;

import com.example.dto.Order;
import com.example.dto.Result;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestParam;
import org.springframework.web.bind.annotation.RestController;

import java.util.List;

/**
 * 订单查询。
 */
@RestController
@RequestMapping("/orders")
public class OrderController extends BaseController {

    /**
     * 订单列表。
     */
    @GetMapping("/list")
    public Result<List<Order>> list(@RequestParam(value = "page", defaultValue = "1") int page,
                                    @RequestParam(value = "size", defaultValue = "20") int size) {
        return Result.ok(findOrders(page, size));
    }

    /**
     * 订单详情。
     */
    @GetMapping("/{orderId}")
    public Result<Order> detail(@PathVariable("orderId") Long orderId) {
        return Result.ok(load(orderId));
    }
}
