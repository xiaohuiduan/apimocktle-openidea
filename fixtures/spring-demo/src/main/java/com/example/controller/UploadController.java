package com.example.controller;

import com.example.dto.Result;
import org.springframework.http.MediaType;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestPart;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.multipart.MultipartFile;

/**
 * 附件上传。
 */
@RestController
@RequestMapping("/api/upload")
public class UploadController {

    /**
     * 上传一个文件及其元数据。
     */
    @PostMapping(value = "/file", consumes = MediaType.MULTIPART_FORM_DATA_VALUE)
    public Result<String> upload(@RequestPart("file") MultipartFile file,
                                 @RequestPart(value = "metadata", required = false) String metadata) {
        return Result.ok(file.getOriginalFilename() + metadata);
    }
}
