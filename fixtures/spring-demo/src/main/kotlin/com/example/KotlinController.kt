package com.example

import com.example.dto.Order
import com.example.dto.Result
import java.util.concurrent.atomic.AtomicLong
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PathVariable
import org.springframework.web.bind.annotation.RequestMapping
import org.springframework.web.bind.annotation.RequestParam
import org.springframework.web.bind.annotation.RestController

/**
 * Kotlin 侧的健康检查与订单接口。
 */
@RestController
@RequestMapping("/api/kotlin")
class KotlinController(private val service: OrderService) {

    /** 计数器：字段初始化器出现在方法之前，是最常见的 Kotlin 写法。 */
    private val hits = AtomicLong()

    /**
     * 健康检查。
     */
    @GetMapping("/health")
    fun health(): Result<String> {
        hits.incrementAndGet()
        return Result.ok("up")
    }

    /**
     * 订单快照，返回类型写在参数列表之后。
     */
    @GetMapping("/orders/{id}")
    fun order(@PathVariable("id") id: Long,
              @RequestParam(value = "detail", required = false) detail: Boolean): Result<Order> =
        Result.ok(service.first())
}

/**
 * 构造器注入的服务，只用来让样本真实。
 */
class OrderService {

    fun first(): Order = Order()
}
