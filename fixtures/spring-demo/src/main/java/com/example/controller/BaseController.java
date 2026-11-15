package com.example.controller;

import com.example.dto.Order;
import org.springframework.web.bind.annotation.ModelAttribute;
import org.springframework.web.bind.annotation.RequestMapping;

import java.util.ArrayList;
import java.util.List;

/**
 * 订单接口的公共前缀与辅助方法。
 *
 * <p>子类自己声明类级 {@code @RequestMapping}：Spring MVC 只取最近的类级注解，
 * 这里的 {@code /api} 不会叠加到子类路径上。
 */
@RequestMapping("/api")
public abstract class BaseController {

    /**
     * 当前请求的租户。
     */
    protected String tenant() {
        return "default";
    }

    /**
     * 带 {@code @ModelAttribute} 的方法，不属于任何路由。
     */
    @ModelAttribute("tenant")
    protected String tenantAttribute() {
        return tenant();
    }

    protected List<Order> findOrders(int page, int size) {
        return new ArrayList<>();
    }

    protected Order load(long orderId) {
        return null;
    }
}
