package com.example.client;

import com.example.dto.Order;
import com.example.dto.Result;
import org.springframework.cloud.openfeign.FeignClient;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.PostMapping;

/**
 * 乘客服务，经服务发现调用。
 *
 * <p>{@code name} 是注册中心里的服务名，只有 {@code path} 参与路由前缀，
 * 所以导出的路径里不应出现 {@code passenger-service}。
 */
@FeignClient(name = "passenger-service", path = "/api/passenger")
public interface PassengerClient {

    /**
     * 查询乘客。
     */
    @GetMapping("/{id}")
    Result<Order> get(@PathVariable("id") Long id);

    /**
     * 保存乘客；Feign 接口的入参常常不带绑定注解。
     */
    @PostMapping("/save")
    Result<Order> save(Order order);
}
