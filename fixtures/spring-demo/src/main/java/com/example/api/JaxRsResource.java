package com.example.api;

import com.example.dto.Order;
import jakarta.ws.rs.FormParam;
import jakarta.ws.rs.GET;
import jakarta.ws.rs.POST;
import jakarta.ws.rs.Path;
import jakarta.ws.rs.PathParam;
import jakarta.ws.rs.Produces;
import jakarta.ws.rs.core.MediaType;

/**
 * JAX-RS 资源：动词与路径分属两个注解。
 */
@Path("/api/jaxrs/orders")
public class JaxRsResource {

    /**
     * 订单详情。
     */
    @GET
    @Path("/{id}")
    @Produces(MediaType.APPLICATION_JSON)
    public Order get(@PathParam("id") long id) {
        return null;
    }

    /**
     * 以表单提交一条订单。
     */
    @POST
    @Produces(MediaType.APPLICATION_JSON)
    public Order create(@FormParam("reason") String reason) {
        return null;
    }
}
